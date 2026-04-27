use crate::commands::chat::{run_agent_headless, HeadlessRunOptions, SESSION_SOURCE_PISCI_POOL};
use crate::commands::config::scene::SceneKind;
use crate::notify::{
    dispatch_notification, NotificationLevel, NotificationRequest, NotificationTarget, NotifierDeps,
};
use crate::pool::bridge;
use crate::pool::KoiTodo;
use crate::store::AppState;
pub use pisci_core::heartbeat::{
    build_pool_heartbeat_message, collect_pool_attention, PoolAttention,
};
use pisci_core::project_state::ProjectDecision;
use tracing::warn;

const HEARTBEAT_SOURCE: &str = crate::commands::chat::SESSION_SOURCE_PISCI_HEARTBEAT_GLOBAL;
const HEARTBEAT_POOL_SOURCE: &str = SESSION_SOURCE_PISCI_POOL;
const HEARTBEAT_GLOBAL_SESSION_ID: &str = "pisci_heartbeat_global";

async fn run_mechanical_pool_recovery(state: &AppState) -> Result<Vec<String>, String> {
    let pools = {
        let db = state.db.lock().await;
        db.list_pool_sessions().map_err(|e| e.to_string())?
    };
    let mut notes = Vec::new();

    for pool in pools.into_iter().filter(|pool| pool.status == "active") {
        let activated = bridge::activate_pending_todos(&state.app_handle, state, Some(&pool.id))
            .await
            .map_err(|e| e.to_string())?;
        if activated > 0 {
            notes.push(format!(
                "Mechanical recovery activated {} pending todo(s) in pool '{}'.",
                activated, pool.name
            ));
        }
    }

    Ok(notes)
}

pub async fn scan_attention_pools(state: &AppState) -> Result<Vec<PoolAttention>, String> {
    let cursor_snapshot = {
        let cursor = state.pisci_heartbeat_cursor.lock().await;
        cursor.clone()
    };

    let (pools, all_todos, koi_ids) = {
        let db = state.db.lock().await;
        let pools = db.list_pool_sessions().map_err(|e| e.to_string())?;
        let todos = db.list_koi_todos(None).map_err(|e| e.to_string())?;
        let koi_ids = db
            .list_kois()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|k| k.id)
            .collect::<Vec<_>>();
        (pools, todos, koi_ids)
    };

    let mut attentions = Vec::new();
    let mut advance_cursors = Vec::new();

    for pool in pools.into_iter().filter(|p| p.status != "archived") {
        let messages = {
            let db = state.db.lock().await;
            db.get_pool_messages(&pool.id, 200, 0)
                .map_err(|e| e.to_string())?
        };
        let pool_todos: Vec<KoiTodo> = all_todos
            .iter()
            .filter(|t| t.pool_session_id.as_deref() == Some(pool.id.as_str()))
            .cloned()
            .collect();
        let last_seen = cursor_snapshot.get(&pool.id).copied().unwrap_or(0);
        let latest_message_id = messages.last().map(|m| m.id).unwrap_or(last_seen);

        if let Some(attention) =
            collect_pool_attention(&pool, &messages, &pool_todos, &koi_ids, last_seen)
        {
            attentions.push(attention);
        } else if latest_message_id > last_seen {
            advance_cursors.push((pool.id.clone(), latest_message_id));
        }
    }

    if !advance_cursors.is_empty() {
        let mut cursor = state.pisci_heartbeat_cursor.lock().await;
        for (pool_id, latest_message_id) in advance_cursors {
            cursor.insert(pool_id, latest_message_id);
        }
    }

    attentions.sort_by_key(|a| a.latest_message_id);
    Ok(attentions)
}

pub async fn ensure_heartbeat_session(
    state: &AppState,
    session_id: &str,
    title: &str,
    source: &str,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.ensure_im_session(session_id, title, source)
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn dispatch_heartbeat(
    state: &AppState,
    base_prompt: &str,
    channel: &str,
) -> Result<(), String> {
    if base_prompt.trim().is_empty() {
        return Ok(());
    }
    let recovery_notes = run_mechanical_pool_recovery(state).await?;
    let attentions = scan_attention_pools(state).await?;
    if attentions.is_empty() {
        ensure_heartbeat_session(
            state,
            HEARTBEAT_GLOBAL_SESSION_ID,
            "Pisci Heartbeat",
            HEARTBEAT_SOURCE,
        )
        .await?;
        run_agent_headless(
            state,
            HEARTBEAT_GLOBAL_SESSION_ID,
            &if recovery_notes.is_empty() {
                base_prompt.to_string()
            } else {
                format!(
                    "{}\n\n## Mechanical Recovery Actions\n{}",
                    base_prompt,
                    recovery_notes
                        .iter()
                        .map(|note| format!("- {}", note))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            },
            None,
            channel,
            Some(HeadlessRunOptions {
                session_title: Some("Pisci Heartbeat".into()),
                session_source: Some(HEARTBEAT_SOURCE.into()),
                scene_kind: Some(SceneKind::HeartbeatSupervisor),
                ..HeadlessRunOptions::default()
            }),
        )
        .await
        .map(|_| ())
    } else {
        for attention in attentions {
            ensure_heartbeat_session(
                state,
                &attention.session_id,
                &format!("Pisci · {}", attention.pool_name),
                HEARTBEAT_POOL_SOURCE,
            )
            .await?;

            // Safety-net: surface critical human-escalation states to the user via a
            // toast in the main UI even if Pisci's own turn fails or is delayed.
            if matches!(
                attention.assessment.decision,
                ProjectDecision::EscalateToHuman
            ) {
                emit_auto_escalation_toast(state, &attention).await;
            }

            let heartbeat_message = build_pool_heartbeat_message(base_prompt, &attention);
            run_agent_headless(
                state,
                &attention.session_id,
                &heartbeat_message,
                None,
                channel,
                Some(HeadlessRunOptions {
                    pool_session_id: Some(attention.pool_id.clone()),
                    extra_system_context: Some(format!(
                        "You are reviewing pool '{}' ({}) during a heartbeat scan.\n\
                         Assessment: {} | Decision: {:?}\n\
                         \n\
                         Available coordination tools: pool_org (list, get_todos, get_messages, post_status, resume_todo, assign_koi, merge_branches, etc.).\n\
                         Do not use pool_chat from heartbeat; Pisci heartbeat communicates through pool_org-controlled actions.\n\
                         If any todo is needs_review, stable state is not enough: inspect messages/todos and either close it out, route rework, or post a concrete status explaining the blocker.\n\
                         If the pool has a project_dir and branches need merging, consider using merge_branches.\n\
                         During heartbeat, NEVER archive a pool automatically — only the user can explicitly request archiving.\n\
                         Reply HEARTBEAT_OK only after the review state has been handled, not merely because there were no new messages.",
                        attention.pool_name,
                        attention.pool_id,
                        attention.assessment.summary,
                        attention.assessment.decision,
                    )),
                    session_title: Some(format!("Pisci · {}", attention.pool_name)),
                    session_source: Some(HEARTBEAT_POOL_SOURCE.into()),
                    scene_kind: Some(SceneKind::HeartbeatSupervisor),
                    ..HeadlessRunOptions::default()
                }),
            )
            .await?;
            let mut cursor = state.pisci_heartbeat_cursor.lock().await;
            cursor.insert(attention.pool_id.clone(), attention.latest_message_id);
        }
        Ok(())
    }
}

/// Emit a `pisci_toast` event as a human-escalation safety net. This runs
/// before Pisci's own turn so the user is alerted even if Pisci itself fails
/// or takes a long time to respond. Pisci is still expected to call
/// `app_control(notify_user, ...)` itself to add a diagnostic summary.
///
/// When the pool was created from an IM conversation
/// (`pool_sessions.origin_im_binding_key`), the same notification is
/// fanned out to that IM channel so users who interact with Pisci
/// remotely don't miss escalations while the desktop UI is closed.
async fn emit_auto_escalation_toast(state: &AppState, attention: &PoolAttention) {
    let reasons = if attention.assessment.attention_reasons.is_empty() {
        attention.assessment.summary.clone()
    } else {
        attention.assessment.attention_reasons.join("; ")
    };
    let preview: String = reasons.chars().take(240).collect();
    let title = format!("需要人工决策 · {}", attention.pool_name);

    let origin_binding = {
        let db = state.db.lock().await;
        match db.get_pool_session(&attention.pool_id) {
            Ok(Some(pool)) => pool.origin_im_binding_key,
            Ok(None) => None,
            Err(err) => {
                warn!(
                    "auto-escalation: failed to load pool {} for IM origin lookup: {}",
                    attention.pool_id, err
                );
                None
            }
        }
    };

    let mut request = NotificationRequest::new(title, preview)
        .with_level(NotificationLevel::Critical)
        .with_source("heartbeat_auto")
        .with_pool(attention.pool_id.clone())
        .with_duration_ms(0)
        .add_target(NotificationTarget::Ui);
    if let Some(binding_key) = origin_binding {
        request = request.add_target(NotificationTarget::im_binding(binding_key));
    }

    let deps = NotifierDeps::from_state(state);
    let outcomes = dispatch_notification(&deps, request).await;
    for outcome in outcomes.iter().filter(|o| !o.delivered) {
        warn!(
            "auto-escalation: failed to deliver to {} for pool {}: {}",
            outcome.target.to_token(),
            attention.pool_id,
            outcome.detail
        );
    }
}
