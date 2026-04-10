use crate::koi::{KoiTodo, PoolMessage};
use std::collections::{HashMap, HashSet};

pub const STATUS_FOLLOW_UP: &str = "[projectstatus] follow_up_needed";
pub const STATUS_WAITING: &str = "[projectstatus] waiting";
pub const STATUS_READY: &str = "[projectstatus] ready_for_pisci_review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDecision {
    Continue,
    ReadyForPisciReview,
}

#[derive(Debug, Clone)]
pub struct ProjectAssessment {
    pub decision: ProjectDecision,
    pub active_todo_count: usize,
    pub blocked_todo_count: usize,
    pub follow_up_signal_count: usize,
    pub ready_signal_count: usize,
    pub explicit_pisci_handoff_count: usize,
    pub summary: String,
}

#[derive(Debug, Clone, Copy)]
struct SenderState {
    signal: &'static str,
    mentions_pisci: bool,
}

pub fn extract_project_status_signal(content: &str) -> Option<&'static str> {
    let lower = content.trim().to_lowercase();
    if lower.contains(STATUS_FOLLOW_UP) {
        Some(STATUS_FOLLOW_UP)
    } else if lower.contains(STATUS_WAITING) {
        Some(STATUS_WAITING)
    } else if lower.contains(STATUS_READY) {
        Some(STATUS_READY)
    } else {
        None
    }
}

pub fn contains_pisci_mention(content: &str) -> bool {
    content.to_lowercase().contains("@pisci")
}

pub fn assess_project_state(
    messages: &[PoolMessage],
    todos: &[KoiTodo],
    koi_ids: &[String],
) -> ProjectAssessment {
    let active_todos: Vec<_> = todos
        .iter()
        .filter(|t| matches!(t.status.as_str(), "todo" | "in_progress" | "blocked"))
        .collect();
    let blocked_todo_count = active_todos
        .iter()
        .filter(|t| t.status == "blocked")
        .count();
    let active_todo_count = active_todos.len();

    // Count recent task_failed events (from any Koi) — these indicate a Koi
    // crashed or timed out and Pisci should intervene even if no @pisci mention
    // was explicitly sent.
    let recent_task_failed_count = messages
        .iter()
        .filter(|m| m.event_type.as_deref() == Some("task_failed"))
        .count();

    let koi_id_set: HashSet<&str> = koi_ids.iter().map(|s| s.as_str()).collect();
    let mut latest_signals: HashMap<String, SenderState> = HashMap::new();
    let mut ordered_messages: Vec<&PoolMessage> = messages
        .iter()
        .filter(|m| koi_id_set.contains(m.sender_id.as_str()))
        .collect();
    ordered_messages.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    for msg in ordered_messages {
        if let Some(signal) = extract_project_status_signal(&msg.content) {
            latest_signals.insert(
                msg.sender_id.clone(),
                SenderState {
                    signal,
                    mentions_pisci: contains_pisci_mention(&msg.content),
                },
            );
        }
    }

    let follow_up_signal_count = latest_signals
        .values()
        .filter(|s| matches!(s.signal, STATUS_FOLLOW_UP | STATUS_WAITING))
        .count();
    let ready_states: Vec<_> = latest_signals
        .values()
        .filter(|s| s.signal == STATUS_READY)
        .copied()
        .collect();
    let ready_signal_count = ready_states.len();
    let explicit_pisci_handoff_count = ready_states.iter().filter(|s| s.mentions_pisci).count();

    if active_todo_count > 0 {
        let summary = if blocked_todo_count > 0 {
            let failure_hint = if recent_task_failed_count > 0 {
                format!(
                    " ({} task_failed event(s) detected — a Koi may have timed out or crashed)",
                    recent_task_failed_count
                )
            } else {
                String::new()
            };
            format!(
                "Project still has {} active todo(s), including {} blocked todo(s){}. Pisci must intervene to unblock or reassign.",
                active_todo_count, blocked_todo_count, failure_hint
            )
        } else {
            format!(
                "Project still has {} active todo(s). More work is in progress before Pisci should conclude.",
                active_todo_count
            )
        };
        return ProjectAssessment {
            decision: ProjectDecision::Continue,
            active_todo_count,
            blocked_todo_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            summary,
        };
    }

    if follow_up_signal_count > 0 {
        return ProjectAssessment {
            decision: ProjectDecision::Continue,
            active_todo_count,
            blocked_todo_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            summary: format!(
                "{} agent(s) signalled follow-up work or waiting state. Pisci should not conclude yet.",
                follow_up_signal_count
            ),
        };
    }

    if explicit_pisci_handoff_count > 0 {
        return ProjectAssessment {
            decision: ProjectDecision::ReadyForPisciReview,
            active_todo_count,
            blocked_todo_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            summary: format!(
                "{} ready-for-review handoff(s) explicitly mentioned @pisci and no active todos remain. Pisci can now judge whether the project is ready to wrap up.",
                explicit_pisci_handoff_count
            ),
        };
    }

    // All todos are done/cancelled and no follow-up signals — project is ready for review
    if !todos.is_empty() && active_todo_count == 0 {
        return ProjectAssessment {
            decision: ProjectDecision::ReadyForPisciReview,
            active_todo_count,
            blocked_todo_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            summary: "All todos are done or cancelled and no follow-up signals remain. Pisci should archive the project."
                .to_string(),
        };
    }

    let summary = if ready_signal_count > 0 {
        format!(
            "{} ready-for-review signal(s) were observed, but none explicitly handed off to @pisci yet.",
            ready_signal_count
        )
    } else {
        "No clear completion signal observed yet.".to_string()
    };

    ProjectAssessment {
        decision: ProjectDecision::Continue,
        active_todo_count,
        blocked_todo_count,
        follow_up_signal_count,
        ready_signal_count,
        explicit_pisci_handoff_count,
        summary,
    }
}
