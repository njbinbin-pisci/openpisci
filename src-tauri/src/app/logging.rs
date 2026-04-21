//! Platform logging and crash reporting.
//!
//! Extracted from the old monolithic `desktop_app.rs`. Everything here is
//! process-global initialization plumbing that runs exactly once at startup.

use tracing_subscriber::prelude::*;

pub fn default_app_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.pisci.desktop")
}

/// Returns the platform log directory: `<data_dir>/logs`.
/// Falls back to `./logs` only if the platform path is unavailable.
pub fn log_dir() -> std::path::PathBuf {
    let base = default_app_data_dir();
    if !base.as_os_str().is_empty() {
        return base.join("logs");
    }
    std::path::PathBuf::from(".").join("logs")
}

/// Initialise structured logging:
/// - STDERR: human-readable, filtered by RUST_LOG / default "info"
/// - Rolling file: JSON, one file per day, kept up to 7 days (via tracing-appender)
///
/// Returns the `_guard` that must stay alive for the lifetime of the process
/// to ensure the non-blocking writer flushes on drop.
pub fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);

    let file_appender = tracing_appender::rolling::daily(&dir, "pisci.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "pisci_desktop_lib=debug,info".into());

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr);

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_current_span(true)
        .with_span_list(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    guard
}

/// Install a panic hook that writes a crash report to the log directory and
/// re-raises the default panic message so the OS crash dialog still appears.
pub fn install_crash_reporter() {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);

    std::panic::set_hook(Box::new(move |info| {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".into());

        let report = serde_json::json!({
            "event": "panic",
            "timestamp": timestamp,
            "location": location,
            "message": payload,
        });

        let crash_file = dir.join(format!(
            "crash-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        let _ = std::fs::write(&crash_file, report.to_string());

        tracing::error!(
            location = %location,
            message = %payload,
            "PANIC — crash report written to {}",
            crash_file.display()
        );

        eprintln!("PANIC at {location}: {payload}");
    }));
}
