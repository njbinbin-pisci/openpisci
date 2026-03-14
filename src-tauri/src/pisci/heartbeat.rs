use crate::commands::chat::{run_agent_headless, HeadlessRunOptions};
use crate::koi::{KoiTodo, PoolMessage, PoolSession};
use crate::pisci::project_state::{
    assess_project_state, contains_pisci_mention, extract_project_status_signal, ProjectAssessment,
    ProjectDecision,
};
use crate::store::AppState;

const HEARTBEAT_SOURCE: &str = crate::commands::chat::SESSION_SOURCE_PISCI_INBOX_GLOBAL;
const HEARTBEAT_POOL_SOURCE: &str = crate::commands::chat::SESSION_SOURCE_PISCI_INBOX_POOL;
const HEARTBEAT_GLOBAL_SESSION_ID: &str = "pisci_inbox_global";

#[derive(Debug, Clone)]
pub struct PoolAttention {
    pub pool_id: String,
    pub pool_name: String,
    pub latest_message_id: i64,
    pub session_id: String,
    pub summary: String,
    pub assessment: ProjectAssessment,
}

fn preview_chars(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    format!("{}...", content.chars().take(max_chars).collect::<String>())
}

fn is_attention_event(msg: &PoolMessage, koi_ids: &[String]) -> bool {
    if msg.sender_id == "pisci" {
        return false;
    }
    let from_known_koi = koi_ids.iter().any(|id| id == &msg.sender_id);
    if contains_pisci_mention(&msg.content) {
        return true;
    }
    if from_known_koi && extract_project_status_signal(&msg.content).is_some() {
        return true;
    }
    matches!(
        msg.event_type.as_deref(),
        Some("task_completed" | "task_failed" | "task_claimed" | "task_blocked" | "task_cancelled")
    )
}

pub(crate) fn build_pool_heartbeat_message(base_prompt: &str, attention: &PoolAttention) -> String {
    let assessment = &attention.assessment;
    let mut lines = vec![
        base_prompt.to_string(),
        String::new(),
        "## Heartbeat Inbox".to_string(),
        attention.summary.clone(),
        String::new(),
        "## Heartbeat Rules".to_string(),
    ];

    match assessment.decision {
        ProjectDecision::Continue => {
            lines.push(
                "- The project is NOT ready for HEARTBEAT_OK. Do not summarize it as finished."
                    .to_string(),
            );
            if assessment.active_todo_count == 0 && assessment.follow_up_signal_count > 0 {
                lines.push(
                    "- Follow-up work was explicitly signalled, but no active todo remains. Treat this as a coordination stall and re-open the project instead of concluding it."
                        .to_string(),
                );
                lines.push(
                    "- Read the latest pool chat, identify the unresolved work, then use pool_chat / pool_org to assign the next owner or ask the blocking question."
                        .to_string(),
                );
            } else if assessment.active_todo_count > 0 {
                lines.push(
                    "- Active todos still exist. Inspect whether Pisci should unblock, reprioritize, or simply acknowledge progress in pool_chat."
                        .to_string(),
                );
            } else {
                lines.push(
                    "- There is still no clear handoff back to Pisci. Review the latest pool context and keep the project moving rather than closing it."
                        .to_string(),
                );
            }
        }
        ProjectDecision::ReadyForPisciReview => {
            lines.push(
                "- All todos are done or cancelled. The project appears complete. HEARTBEAT_OK is still not automatic."
                    .to_string(),
            );
            lines.push(
                "- Review the pool chat to confirm there is no outstanding work, then execute pool_org(action=\"archive\", pool_id=...) to archive the project and post a wrap-up summary in pool_chat."
                    .to_string(),
            );
            lines.push(
                "- Only skip archiving if you find clear evidence of unresolved work not captured in todos."
                    .to_string(),
            );
        }
    }

    lines.push(
        "Respond in the pool context. Read the pool chat, inspect todos, decide whether Pisci should intervene, and reply in pool_chat when appropriate."
            .to_string(),
    );
    lines.join("\n")
}

pub fn collect_pool_attention(
    pool: &PoolSession,
    messages: &[PoolMessage],
    todos: &[KoiTodo],
    koi_ids: &[String],
    last_seen_message_id: i64,
) -> Option<PoolAttention> {
    let latest_message_id = messages
        .last()
        .map(|m| m.id)
        .unwrap_or(last_seen_message_id);
    let new_attention_messages: Vec<&PoolMessage> = messages
        .iter()
        .filter(|m| m.id > last_seen_message_id && is_attention_event(m, koi_ids))
        .collect();

    let assessment = assess_project_state(messages, todos, koi_ids);

    // Always wake Pisci when all todos are done — even if no new attention events arrived
    // (the last complete_todo event may have already been consumed by a prior heartbeat)
    if new_attention_messages.is_empty()
        && assessment.decision != ProjectDecision::ReadyForPisciReview
    {
        return None;
    }
    let mut lines = vec![
        format!("Pool: {} ({})", pool.name, pool.id),
        format!("Status: {}", pool.status),
        format!("Recent attention events: {}", new_attention_messages.len()),
        format!("Assessment: {}", assessment.summary),
    ];
    if let Some(project_dir) = pool.project_dir.as_deref() {
        lines.push(format!("Project dir: {}", project_dir));
    }
    lines.push("Recent pool events:".to_string());
    for msg in new_attention_messages.iter().rev().take(6).rev() {
        let event = msg.event_type.as_deref().unwrap_or(&msg.msg_type);
        lines.push(format!(
            "- #{} [{}] {}: {}",
            msg.id,
            event,
            msg.sender_id,
            preview_chars(&msg.content.replace('\n', " "), 240)
        ));
    }

    Some(PoolAttention {
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        latest_message_id,
        session_id: format!("pisci_pool_{}", pool.id),
        summary: lines.join("\n"),
        assessment,
    })
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

async fn ensure_heartbeat_session(
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
            base_prompt,
            None,
            channel,
            Some(HeadlessRunOptions {
                session_title: Some("Pisci Heartbeat".into()),
                session_source: Some(HEARTBEAT_SOURCE.into()),
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
                        "You are reviewing the project pool '{}' ({}) during a heartbeat-triggered inbox scan.\nCurrent assessment: {}\nDecision: {:?}\nIf the decision is Continue, you must not emit HEARTBEAT_OK or describe the project as complete.",
                        attention.pool_name, attention.pool_id, attention.assessment.summary, attention.assessment.decision
                    )),
                    session_title: Some(format!("Pisci · {}", attention.pool_name)),
                    session_source: Some(HEARTBEAT_POOL_SOURCE.into()),
                }),
            )
            .await?;
            let mut cursor = state.pisci_heartbeat_cursor.lock().await;
            cursor.insert(attention.pool_id.clone(), attention.latest_message_id);
        }
        Ok(())
    }
}
