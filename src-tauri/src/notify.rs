//! Host-side notification dispatcher.
//!
//! Implements [`pisci_kernel::notify`] semantics for the desktop host:
//! UI toasts go through the Tauri `pisci_toast` event, IM targets are
//! resolved against the persisted `im_session_bindings` table and sent
//! via the existing [`crate::gateway::GatewayManager`].
//!
//! The kernel side stays UI-neutral; everything that touches an
//! `AppHandle`, `tauri::Emitter`, or the gateway lives here.

use crate::gateway;
use crate::store::Database;
pub use pisci_kernel::notify::{
    NotificationLevel, NotificationOutcome, NotificationRequest, NotificationTarget,
};
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Lightweight bundle of host dependencies the dispatcher needs.
///
/// Constructed on demand by call sites (chat tools, scheduler, pool
/// alerts, …) so we do not have to thread a full `AppState` through
/// every code path. Missing fields gracefully degrade: a dispatcher
/// with no `gateway` will refuse IM targets with a clear error
/// outcome instead of panicking.
#[derive(Clone, Default)]
pub struct NotifierDeps {
    pub app_handle: Option<AppHandle>,
    pub gateway: Option<Arc<gateway::GatewayManager>>,
    pub db: Option<Arc<Mutex<Database>>>,
}

impl NotifierDeps {
    pub fn new(
        app_handle: Option<AppHandle>,
        gateway: Option<Arc<gateway::GatewayManager>>,
        db: Option<Arc<Mutex<Database>>>,
    ) -> Self {
        Self {
            app_handle,
            gateway,
            db,
        }
    }

    /// Convenience constructor pulling everything from the global
    /// [`crate::store::AppState`].
    pub fn from_state(state: &crate::store::AppState) -> Self {
        Self {
            app_handle: Some(state.app_handle.clone()),
            gateway: Some(state.gateway.clone()),
            db: Some(state.db.clone()),
        }
    }
}

/// Fan a [`NotificationRequest`] out to every requested target. The
/// caller receives a per-target outcome list so it can decide how to
/// surface failures.
///
/// If the request has no explicit targets, the dispatcher applies a
/// UI-only fallback so legacy callers keep their original behaviour.
pub async fn dispatch_notification(
    deps: &NotifierDeps,
    mut request: NotificationRequest,
) -> Vec<NotificationOutcome> {
    if request.targets.is_empty() {
        request.targets.push(NotificationTarget::Ui);
    }
    request.dedup_targets();

    let mut outcomes = Vec::with_capacity(request.targets.len());
    for target in request.targets.clone() {
        let outcome = match &target {
            NotificationTarget::Ui => emit_ui_toast(deps, &request),
            NotificationTarget::ImBinding { binding_key } => {
                send_to_im_binding(deps, &request, binding_key).await
            }
            NotificationTarget::ImSession { session_id } => {
                send_to_im_session(deps, &request, session_id).await
            }
        };
        outcomes.push(outcome);
    }
    outcomes
}

fn build_toast_payload(request: &NotificationRequest) -> serde_json::Value {
    let id = format!(
        "toast_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4().simple()
    );
    json!({
        "id": id,
        "title": request.title,
        "message": request.message,
        "level": request.level.as_str(),
        "pool_id": request.pool_id,
        "decision_id": request.decision_id,
        "duration_ms": request.effective_duration_ms(),
        "source": if request.source.is_empty() {
            "pisci".to_string()
        } else {
            request.source.clone()
        },
        "ts": chrono::Utc::now().timestamp_millis(),
    })
}

fn emit_ui_toast(deps: &NotifierDeps, request: &NotificationRequest) -> NotificationOutcome {
    let Some(app) = deps.app_handle.as_ref() else {
        return NotificationOutcome::failed(
            NotificationTarget::Ui,
            "UI notifier unavailable (no AppHandle)",
        );
    };
    let payload = build_toast_payload(request);
    match app.emit("pisci_toast", payload) {
        Ok(()) => {
            debug!(
                "notify: ui toast emitted level={} title={:?}",
                request.level.as_str(),
                request.title
            );
            NotificationOutcome::ok(NotificationTarget::Ui, "toast emitted")
        }
        Err(err) => {
            warn!("notify: failed to emit pisci_toast: {}", err);
            NotificationOutcome::failed(
                NotificationTarget::Ui,
                format!("failed to emit pisci_toast: {}", err),
            )
        }
    }
}

fn render_im_text(request: &NotificationRequest) -> String {
    let level_tag = match request.level {
        NotificationLevel::Critical => "[CRITICAL] ",
        NotificationLevel::Error => "[ERROR] ",
        NotificationLevel::Warning => "[WARN] ",
        NotificationLevel::Info => "",
    };
    let title = request.title.trim();
    let body = request.message.trim();
    if title.is_empty() {
        format!("{}{}", level_tag, body)
    } else if body.is_empty() {
        format!("{}{}", level_tag, title)
    } else {
        format!("{}{}\n{}", level_tag, title, body)
    }
}

async fn lookup_binding_by_key(
    deps: &NotifierDeps,
    binding_key: &str,
) -> Option<crate::store::db::ImSessionBinding> {
    let db = deps.db.as_ref()?;
    let db = db.lock().await;
    db.get_im_session_binding(binding_key).ok().flatten()
}

async fn lookup_binding_by_session(
    deps: &NotifierDeps,
    session_id: &str,
) -> Option<crate::store::db::ImSessionBinding> {
    let db = deps.db.as_ref()?;
    let db = db.lock().await;
    db.find_im_session_binding_for_session(session_id)
        .ok()
        .flatten()
}

async fn send_to_im_binding(
    deps: &NotifierDeps,
    request: &NotificationRequest,
    binding_key: &str,
) -> NotificationOutcome {
    let target = NotificationTarget::im_binding(binding_key);
    if deps.gateway.is_none() {
        return NotificationOutcome::failed(target, "gateway manager unavailable");
    }
    if deps.db.is_none() {
        return NotificationOutcome::failed(target, "database handle unavailable");
    }
    let binding = match lookup_binding_by_key(deps, binding_key).await {
        Some(b) => b,
        None => {
            return NotificationOutcome::failed(
                target,
                format!("binding '{}' not found", binding_key),
            );
        }
    };
    deliver_via_gateway(deps, request, target, binding).await
}

async fn send_to_im_session(
    deps: &NotifierDeps,
    request: &NotificationRequest,
    session_id: &str,
) -> NotificationOutcome {
    let target = NotificationTarget::im_session(session_id);
    if deps.gateway.is_none() {
        return NotificationOutcome::failed(target, "gateway manager unavailable");
    }
    if deps.db.is_none() {
        return NotificationOutcome::failed(target, "database handle unavailable");
    }
    let binding = match lookup_binding_by_session(deps, session_id).await {
        Some(b) => b,
        None => {
            return NotificationOutcome::failed(
                target,
                format!("no IM binding for session '{}'", session_id),
            );
        }
    };
    deliver_via_gateway(deps, request, target, binding).await
}

async fn deliver_via_gateway(
    deps: &NotifierDeps,
    request: &NotificationRequest,
    target: NotificationTarget,
    binding: crate::store::db::ImSessionBinding,
) -> NotificationOutcome {
    let Some(gateway) = deps.gateway.as_ref() else {
        return NotificationOutcome::failed(target, "gateway manager unavailable");
    };
    let routing_state = binding
        .routing_state_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    let recipient = if binding.latest_reply_target.trim().is_empty() {
        binding.peer_id.clone()
    } else {
        binding.latest_reply_target.clone()
    };
    let outbound = gateway::OutboundMessage {
        channel: binding.channel.clone(),
        recipient: recipient.clone(),
        content: render_im_text(request),
        reply_to: None,
        media: None,
        routing_state,
    };
    match gateway.send(&outbound).await {
        Ok(()) => NotificationOutcome::ok(
            target,
            format!("sent via {} to {}", binding.channel, recipient),
        ),
        Err(err) => {
            warn!(
                "notify: failed to send via channel={} binding_key={}: {}",
                binding.channel, binding.binding_key, err
            );
            NotificationOutcome::failed(
                target,
                format!("gateway send failed ({}): {}", binding.channel, err),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_im_text_includes_level_prefix_only_for_non_info() {
        let info = NotificationRequest::new("Title", "Body").with_level(NotificationLevel::Info);
        let warn = NotificationRequest::new("Title", "Body").with_level(NotificationLevel::Warning);
        let crit =
            NotificationRequest::new("Title", "Body").with_level(NotificationLevel::Critical);
        assert_eq!(render_im_text(&info), "Title\nBody");
        assert_eq!(render_im_text(&warn), "[WARN] Title\nBody");
        assert_eq!(render_im_text(&crit), "[CRITICAL] Title\nBody");
    }

    #[test]
    fn render_im_text_handles_empty_title_or_body() {
        let only_body = NotificationRequest::new("", "  Body  ");
        let only_title = NotificationRequest::new("Title", "");
        assert_eq!(render_im_text(&only_body), "Body");
        assert_eq!(render_im_text(&only_title), "Title");
    }

    #[tokio::test]
    async fn dispatch_without_gateway_reports_failure_for_im_targets() {
        let deps = NotifierDeps::default();
        let request = NotificationRequest::new("t", "b")
            .add_target(NotificationTarget::im_binding("wechat::abc"));
        let outcomes = dispatch_notification(&deps, request).await;
        assert_eq!(outcomes.len(), 1);
        assert!(!outcomes[0].delivered);
        assert!(outcomes[0].detail.contains("gateway"));
    }
}
