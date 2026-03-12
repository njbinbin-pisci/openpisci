/// EventBus trait — abstracts event emission so core logic can run without Tauri.
///
/// Real implementation uses `tauri::AppHandle::emit()`.
/// Test implementation captures events into a log for assertions.

use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub trait EventBus: Send + Sync {
    /// Emit a named event with a JSON payload.
    fn emit_event(&self, event: &str, payload: Value);

    /// Access the shared Database.
    fn db(&self) -> &std::sync::Arc<tokio::sync::Mutex<crate::store::Database>>;

    /// Access the Tauri AppHandle if available (returns None in test mode).
    fn app_handle(&self) -> Option<&tauri::AppHandle> { None }
}

// ---------------------------------------------------------------------------
// Tauri implementation
// ---------------------------------------------------------------------------

pub struct TauriEventBus {
    pub app: tauri::AppHandle,
    pub db_ref: std::sync::Arc<tokio::sync::Mutex<crate::store::Database>>,
}

#[async_trait]
impl EventBus for TauriEventBus {
    fn emit_event(&self, event: &str, payload: Value) {
        use tauri::Emitter;
        let _ = self.app.emit(event, payload);
    }

    fn db(&self) -> &std::sync::Arc<tokio::sync::Mutex<crate::store::Database>> {
        &self.db_ref
    }

    fn app_handle(&self) -> Option<&tauri::AppHandle> {
        Some(&self.app)
    }
}

// ---------------------------------------------------------------------------
// Test / logging implementation
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LogEventBus {
    pub db_ref: std::sync::Arc<tokio::sync::Mutex<crate::store::Database>>,
    pub events: std::sync::Arc<tokio::sync::Mutex<Vec<(String, Value)>>>,
}

impl LogEventBus {
    pub fn new(db: std::sync::Arc<tokio::sync::Mutex<crate::store::Database>>) -> Self {
        Self {
            db_ref: db,
            events: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    pub async fn drain_events(&self) -> Vec<(String, Value)> {
        let mut events = self.events.lock().await;
        std::mem::take(&mut *events)
    }

    pub async fn events_named(&self, name: &str) -> Vec<Value> {
        let events = self.events.lock().await;
        events.iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub async fn event_count(&self) -> usize {
        self.events.lock().await.len()
    }
}

#[async_trait]
impl EventBus for LogEventBus {
    fn emit_event(&self, event: &str, payload: Value) {
        let events = self.events.clone();
        let event = event.to_string();
        match events.try_lock() {
            Ok(mut guard) => { guard.push((event, payload)); }
            Err(_) => {
                let events2 = events.clone();
                tokio::spawn(async move {
                    events2.lock().await.push((event, payload));
                });
            }
        };
    }

    fn db(&self) -> &std::sync::Arc<tokio::sync::Mutex<crate::store::Database>> {
        &self.db_ref
    }
}
