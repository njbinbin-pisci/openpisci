//! Headless host implementation of the pisci-core host traits.
//!
//! The CLI host writes all agent events to stdout as NDJSON (one JSON object
//! per line) so that external tooling (benchmarks, scripts, IDE integrations)
//! can consume agent progress in real time without a Tauri event bus. All
//! interactive prompts resolve to their deterministic defaults because no
//! human is attached.

use std::io::{self, Write};
use std::sync::Mutex;

use pisci_core::host::{
    ConfirmRequest, EventSink, HostRuntime, HostTools, InteractiveRequest, Notifier, SecretsStore,
};
use serde_json::{json, Value};

/// NDJSON event sink that serialises every emission to stdout.
pub struct CliEventSink {
    // stdout writes need to be line-atomic; a mutex is cheap and simple.
    out: Mutex<io::Stdout>,
}

impl Default for CliEventSink {
    fn default() -> Self {
        Self {
            out: Mutex::new(io::stdout()),
        }
    }
}

impl EventSink for CliEventSink {
    fn emit_session(&self, session_id: &str, event: &str, payload: Value) {
        let line = json!({
            "kind": "session_event",
            "session": session_id,
            "event": event,
            "payload": payload,
        });
        self.write_line(&line);
    }

    fn emit_broadcast(&self, event: &str, payload: Value) {
        let line = json!({
            "kind": "broadcast",
            "event": event,
            "payload": payload,
        });
        self.write_line(&line);
    }
}

impl CliEventSink {
    fn write_line(&self, value: &Value) {
        let mut out = match self.out.lock() {
            Ok(guard) => guard,
            // A poisoned stdout lock means another thread panicked while
            // writing; recover best-effort so we still surface the event.
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = writeln!(out, "{}", value);
        let _ = out.flush();
    }
}

/// Default notifier: headless runs have no user to prompt, so all requests
/// resolve to the request's declared default or a benign fallback.
#[derive(Default)]
pub struct CliNotifier;

#[async_trait::async_trait]
impl Notifier for CliNotifier {
    fn toast(&self, level: &str, message: &str, pool_id: Option<&str>, _duration_ms: Option<u64>) {
        // Route toasts to stderr so they are visible in the CLI but do not
        // pollute the NDJSON stream on stdout.
        if let Some(pool) = pool_id {
            eprintln!("[toast/{level}][{pool}] {message}");
        } else {
            eprintln!("[toast/{level}] {message}");
        }
    }

    async fn request_confirmation(&self, req: ConfirmRequest) -> bool {
        // Headless: honour the caller-provided default, otherwise deny so we
        // never auto-approve destructive actions in CI.
        req.default.unwrap_or(false)
    }

    async fn request_interactive(&self, req: InteractiveRequest) -> Value {
        // Headless interactive requests simply echo back the default payload
        // (if any). The kernel side is expected to treat `Null` as "skipped".
        req.default.unwrap_or(Value::Null)
    }
}

/// No-op tool injector: CLI runs do not expose browser/UIA/screen tools.
#[derive(Default)]
pub struct CliHostTools;

impl HostTools for CliHostTools {
    fn register(&self, _registry: &mut pisci_core::host::ToolRegistryHandle) {
        // Intentionally empty — headless runs only use neutral tools already
        // registered by the kernel itself.
    }
}

/// Environment-variable backed secrets store. A real deployment can point
/// this at a config file; for now env-vars are enough for headless CI use.
#[derive(Default)]
pub struct CliSecretsStore;

impl SecretsStore for CliSecretsStore {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn set(&self, _key: &str, _value: &str) -> anyhow::Result<()> {
        // CLI host treats secrets as read-only from the process environment.
        Ok(())
    }
}

/// Aggregate host passed to the kernel. Holds all four adapter singletons.
pub struct CliHost {
    event_sink: std::sync::Arc<CliEventSink>,
    notifier: std::sync::Arc<CliNotifier>,
    host_tools: std::sync::Arc<CliHostTools>,
    secrets: std::sync::Arc<CliSecretsStore>,
    app_data_dir: std::path::PathBuf,
}

impl CliHost {
    pub fn new(app_data_dir: std::path::PathBuf) -> Self {
        Self {
            event_sink: std::sync::Arc::new(CliEventSink::default()),
            notifier: std::sync::Arc::new(CliNotifier),
            host_tools: std::sync::Arc::new(CliHostTools),
            secrets: std::sync::Arc::new(CliSecretsStore),
            app_data_dir,
        }
    }
}

impl HostRuntime for CliHost {
    fn event_sink(&self) -> std::sync::Arc<dyn EventSink> {
        self.event_sink.clone()
    }
    fn notifier(&self) -> std::sync::Arc<dyn Notifier> {
        self.notifier.clone()
    }
    fn host_tools(&self) -> std::sync::Arc<dyn HostTools> {
        self.host_tools.clone()
    }
    fn secrets(&self) -> std::sync::Arc<dyn SecretsStore> {
        self.secrets.clone()
    }
    fn app_data_dir(&self) -> std::path::PathBuf {
        self.app_data_dir.clone()
    }
}
