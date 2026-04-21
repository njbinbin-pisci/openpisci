//! EventBus trait — minimal abstraction so the in-process `call_koi` path
//! can emit UI events without hard-coupling to Tauri in every helper.
//!
//! After Phase 4, pool coordination lives in `pisci-kernel`. The only
//! remaining consumer on the desktop side is `koi::runtime` for the
//! `call_koi` tool, so this module is intentionally small.

use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub trait EventBus: Send + Sync {
    fn emit_event(&self, event: &str, payload: Value);

    fn db(&self) -> &std::sync::Arc<tokio::sync::Mutex<crate::store::Database>>;

    fn app_handle(&self) -> Option<&tauri::AppHandle> {
        None
    }
}

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
