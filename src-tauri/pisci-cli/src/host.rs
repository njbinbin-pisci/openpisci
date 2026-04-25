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
    ConfirmRequest, EventSink, HostRuntime, HostTools, InteractiveRequest, Notifier, PoolEvent,
    PoolEventSink, SecretsStore, SubagentRuntime,
};
use serde_json::{json, Value};

/// NDJSON event sink that serialises every emission to a line-oriented stream.
pub struct CliEventSink {
    // Writes need to be line-atomic; a mutex is cheap and simple.
    out: Mutex<Box<dyn Write + Send>>,
}

impl Default for CliEventSink {
    fn default() -> Self {
        Self::stdout()
    }
}

impl CliEventSink {
    pub fn stdout() -> Self {
        Self {
            out: Mutex::new(Box::new(io::stdout())),
        }
    }

    pub fn stderr() -> Self {
        Self {
            out: Mutex::new(Box::new(io::stderr())),
        }
    }

    fn write_line(&self, value: &Value) {
        let mut out = match self.out.lock() {
            Ok(guard) => guard,
            // A poisoned output lock means another thread panicked while
            // writing; recover best-effort so we still surface the event.
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = writeln!(out, "{}", value);
        let _ = out.flush();
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

// NDJSON wire shape for pool events — one line per emission, same stdout
// stream used for `session_event` / `broadcast`, but with the outer
// `kind` set to `pool_event` so consumers can route on a single field:
//
// ```json
// {"kind":"pool_event","pool_id":"p1","event":"pool_created",
//  "payload":{"kind":"pool_created","pool":{...}}}
// ```
//
// `payload` is the full serialized [`PoolEvent`] (including its own
// `kind` tag), so downstream consumers can deserialize it back into a
// `PoolEvent` without losing any field. `event` and `pool_id` are
// denormalised convenience copies for quick filtering.
impl PoolEventSink for CliEventSink {
    fn emit_pool(&self, event: &PoolEvent) {
        // Fallback to `Value::Null` if serialization somehow fails — better
        // to surface a degraded line than to drop the event silently.
        let payload = serde_json::to_value(event).unwrap_or(Value::Null);
        let line = json!({
            "kind": "pool_event",
            "pool_id": event.pool_id(),
            "event": event.kind(),
            "payload": payload,
        });
        self.write_line(&line);
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

/// Aggregate host passed to the kernel. Holds all four adapter singletons
/// plus (optionally, for pool-mode parents) a [`SubagentRuntime`] used to
/// fan out Koi turns as subprocesses.
pub struct CliHost {
    event_sink: std::sync::Arc<CliEventSink>,
    notifier: std::sync::Arc<CliNotifier>,
    host_tools: std::sync::Arc<CliHostTools>,
    secrets: std::sync::Arc<CliSecretsStore>,
    app_data_dir: std::path::PathBuf,
    subagent_runtime: Option<std::sync::Arc<dyn SubagentRuntime>>,
}

impl CliHost {
    pub fn new(app_data_dir: std::path::PathBuf) -> Self {
        Self::new_with_event_sink(app_data_dir, std::sync::Arc::new(CliEventSink::default()))
    }

    pub fn new_with_event_sink(
        app_data_dir: std::path::PathBuf,
        event_sink: std::sync::Arc<CliEventSink>,
    ) -> Self {
        Self {
            event_sink,
            notifier: std::sync::Arc::new(CliNotifier),
            host_tools: std::sync::Arc::new(CliHostTools),
            secrets: std::sync::Arc::new(CliSecretsStore),
            app_data_dir,
            subagent_runtime: None,
        }
    }

    /// Attach a [`SubagentRuntime`] so the CLI host can drive pool-mode
    /// runs. Without this, `assign_koi` / mention dispatch will surface
    /// "no subagent runtime" errors — appropriate for the pisci-only
    /// path but not for `run --mode pool`.
    pub fn with_subagent_runtime(mut self, runtime: std::sync::Arc<dyn SubagentRuntime>) -> Self {
        self.subagent_runtime = Some(runtime);
        self
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

    fn pool_event_sink(&self) -> std::sync::Arc<dyn PoolEventSink> {
        // `CliEventSink` implements both [`EventSink`] and [`PoolEventSink`]
        // so every emission goes through the same stdout lock — prevents
        // interleaving between pool/session lines at line boundaries.
        self.event_sink.clone()
    }

    fn subagent_runtime(&self) -> Option<std::sync::Arc<dyn SubagentRuntime>> {
        self.subagent_runtime.clone()
    }
}
