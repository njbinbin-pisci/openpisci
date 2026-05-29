//! Graceful + forced application shutdown.
//!
//! Tray quit and other exit paths must cancel long-running agent/IM work and
//! tear down gateway/browser/PTY resources before calling `app.exit`. Without
//! that, Tokio tasks and WebView teardown can stall indefinitely; on Windows the
//! process may ignore normal termination until force-killed.

use crate::store::AppState;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tracing::info;

const SHUTDOWN_PREP_TIMEOUT: Duration = Duration::from_secs(3);
const FORCE_EXIT_BACKSTOP: Duration = Duration::from_secs(4);

/// Cancel agents, stop gateways, and release heavy host resources.
pub async fn prepare_shutdown(state: &AppState) {
    info!("Shutdown: cancelling active agent runs");
    {
        let flags = state.cancel_flags.lock().await;
        for flag in flags.values() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    info!("Shutdown: cancelling UIA calibration phase 2 (if any)");
    crate::commands::platform::calibration::cancel_phase2_if_running().await;

    info!("Shutdown: stopping IM gateway channels");
    let _ = tokio::time::timeout(SHUTDOWN_PREP_TIMEOUT, state.gateway.stop_all()).await;

    info!("Shutdown: closing browser");
    {
        let mut browser = state.browser.lock().await;
        browser.close().await;
    }

    info!("Shutdown: destroying IDE terminals");
    {
        let mut registry = state.terminals.lock().await;
        let ids: Vec<String> = registry.sessions.keys().cloned().collect();
        for id in ids {
            if let Some(mut session) = registry.sessions.remove(&id) {
                session.writer.take();
                let _ = session.child.kill();
            }
        }
    }

    info!("Shutdown: clearing file watchers");
    {
        let mut watchers = state.file_watchers.lock().await;
        watchers.clear();
    }

    info!("Shutdown: stopping LSP sessions");
    state.lsp_manager.stop_all().await;

    info!("Shutdown: prepare complete");
}

/// Begin shutdown from the UI thread (tray menu, overlay quit, etc.).
pub fn request_app_exit(app: AppHandle) {
    info!("Shutdown requested");

    // If graceful teardown or `app.exit` stalls, still terminate the process.
    std::thread::spawn({
        let app_backstop = app.clone();
        move || {
            std::thread::sleep(FORCE_EXIT_BACKSTOP);
            info!("Shutdown: force-exit backstop");
            app_backstop.exit(0);
            std::process::exit(0);
        }
    });

    tauri::async_runtime::spawn(async move {
        let prep = async {
            if let Some(state) = app.try_state::<AppState>() {
                prepare_shutdown(&state).await;
            }
        };
        let _ = tokio::time::timeout(SHUTDOWN_PREP_TIMEOUT + Duration::from_secs(1), prep).await;
        info!("Shutdown: calling app.exit(0)");
        app.exit(0);
    });
}
