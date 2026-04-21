use crate::models::{KoiTodo, PoolMessage};
use crate::scene::EventDigestMode;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const STATUS_FOLLOW_UP: &str = "[projectstatus] follow_up_needed";
pub const STATUS_WAITING: &str = "[projectstatus] waiting";
pub const STATUS_READY: &str = "[projectstatus] ready_for_pisci_review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDecision {
    Continue,
    SupervisorDecisionRequired,
    EscalateToHuman,
    ReadyForPisciReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinationSignalKind {
    FollowUpNeeded,
    Waiting,
    ReadyForPisciReview,
}

impl CoordinationSignalKind {
    pub fn as_status_str(self) -> &'static str {
        match self {
            CoordinationSignalKind::FollowUpNeeded => STATUS_FOLLOW_UP,
            CoordinationSignalKind::Waiting => STATUS_WAITING,
            CoordinationSignalKind::ReadyForPisciReview => STATUS_READY,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectAssessment {
    pub decision: ProjectDecision,
    pub active_todo_count: usize,
    pub blocked_todo_count: usize,
    pub needs_review_count: usize,
    pub task_failed_count: usize,
    pub follow_up_signal_count: usize,
    pub ready_signal_count: usize,
    pub explicit_pisci_handoff_count: usize,
    pub attention_reasons: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoordinationEventDigest {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct SenderState {
    signal: CoordinationSignalKind,
    mentions_pisci: bool,
}

pub fn extract_project_status_signal(content: &str) -> Option<&'static str> {
    detect_coordination_signal(content).map(|signal| signal.as_status_str())
}

pub fn detect_coordination_signal(content: &str) -> Option<CoordinationSignalKind> {
    let lower = content.trim().to_lowercase();
    if lower.starts_with(STATUS_FOLLOW_UP) {
        Some(CoordinationSignalKind::FollowUpNeeded)
    } else if lower.starts_with(STATUS_WAITING) {
        Some(CoordinationSignalKind::Waiting)
    } else if lower.starts_with(STATUS_READY) {
        Some(CoordinationSignalKind::ReadyForPisciReview)
    } else {
        None
    }
}

pub fn contains_pisci_mention(content: &str) -> bool {
    content.to_lowercase().contains("@pisci")
}

fn parse_message_metadata(metadata: &str) -> Option<Value> {
    serde_json::from_str(metadata).ok()
}

fn coordination_signal_from_metadata(metadata: &str) -> Option<SenderState> {
    let metadata = parse_message_metadata(metadata)?;
    let coordination = metadata.get("coordination")?;
    let signal = match coordination.get("signal").and_then(Value::as_str) {
        Some(STATUS_FOLLOW_UP) => CoordinationSignalKind::FollowUpNeeded,
        Some(STATUS_WAITING) => CoordinationSignalKind::Waiting,
        Some(STATUS_READY) => CoordinationSignalKind::ReadyForPisciReview,
        _ => return None,
    };
    let mentions_pisci = coordination
        .get("mentions_pisci")
        .and_then(Value::as_bool)
        .or_else(|| {
            metadata
                .get("mentions")
                .and_then(|mentions| mentions.get("pisci"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    Some(SenderState {
        signal,
        mentions_pisci,
    })
}

fn extract_sender_state(msg: &PoolMessage) -> Option<SenderState> {
    coordination_signal_from_metadata(&msg.metadata).or_else(|| {
        detect_coordination_signal(&msg.content).map(|signal| SenderState {
            signal,
            mentions_pisci: contains_pisci_mention(&msg.content),
        })
    })
}

fn truncate_chars(content: &str, max_chars: usize) -> String {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return content.to_string();
    }
    format!("{}...", content.chars().take(max_chars).collect::<String>())
}

fn text_mentions_any(content: &str, targets: &[&str]) -> bool {
    if targets.is_empty() {
        return false;
    }
    let lower = content.to_lowercase();
    targets.iter().any(|target| {
        let trimmed = target.trim().trim_start_matches('@').to_lowercase();
        !trimmed.is_empty() && lower.contains(&format!("@{}", trimmed))
    })
}

pub fn enrich_pool_message_metadata(base: Value, content: &str) -> Value {
    let mut metadata = match base {
        Value::Object(map) => Value::Object(map),
        _ => json!({}),
    };
    if contains_pisci_mention(content) {
        if metadata
            .get("mentions")
            .and_then(Value::as_object)
            .is_none()
        {
            metadata["mentions"] = json!({});
        }
        metadata["mentions"]["pisci"] = json!(true);
    }
    if let Some(signal) = detect_coordination_signal(content) {
        metadata["coordination"] = json!({
            "protocol": "project_status_v1",
            "signal": signal.as_status_str(),
            "mentions_pisci": contains_pisci_mention(content),
        });
    }
    metadata
}

pub fn coordination_event_type_for_content(content: &str) -> Option<&'static str> {
    detect_coordination_signal(content).map(|_| "coordination_signal")
}

pub fn build_coordination_event_digest(
    messages: &[PoolMessage],
    mode: EventDigestMode,
    target_mentions: &[&str],
    limit: usize,
    preview_chars: usize,
) -> CoordinationEventDigest {
    if matches!(mode, EventDigestMode::Off) || limit == 0 {
        return CoordinationEventDigest::default();
    }

    let mut lines = Vec::new();
    for msg in messages.iter().rev() {
        let event_type = msg.event_type.as_deref().unwrap_or(msg.msg_type.as_str());
        let is_coordination = matches!(
            msg.event_type.as_deref(),
            Some(
                "coordination_signal"
                    | "task_completed"
                    | "task_blocked"
                    | "task_claimed"
                    | "task_cancelled"
                    | "protocol_warning"
                    | "protocol_reminder"
            )
        ) || extract_sender_state(msg).is_some();
        let is_failure = matches!(msg.event_type.as_deref(), Some("task_failed"));
        let mentions_target = text_mentions_any(&msg.content, target_mentions);
        let keep = match mode {
            EventDigestMode::Off => false,
            EventDigestMode::CoordinationOnly => is_coordination || mentions_target,
            EventDigestMode::CoordinationPlusFailures => {
                is_coordination || is_failure || mentions_target
            }
        };
        if !keep {
            continue;
        }

        let content = truncate_chars(&msg.content.replace('\n', " "), preview_chars);
        let time = msg.created_at.format("%m-%d %H:%M").to_string();
        let mention_tag = if mentions_target { " target" } else { "" };
        lines.push(format!(
            "- [{}] {} [{}{}]: {}",
            time, msg.sender_id, event_type, mention_tag, content
        ));
        if lines.len() >= limit {
            break;
        }
    }
    lines.reverse();
    CoordinationEventDigest { lines }
}

#[allow(clippy::too_many_arguments)]
fn build_attention_reasons(
    unfinished_work_count: usize,
    blocked_todo_count: usize,
    needs_review_count: usize,
    task_failed_count: usize,
    task_failed_still_blocks: bool,
    follow_up_signal_count: usize,
    ready_signal_count: usize,
    explicit_pisci_handoff_count: usize,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if unfinished_work_count > 0 {
        reasons.push(format!(
            "{} unfinished todo(s) remain",
            unfinished_work_count
        ));
    }
    if blocked_todo_count > 0 {
        reasons.push(format!("{} todo(s) are blocked", blocked_todo_count));
    }
    if needs_review_count > 0 {
        reasons.push(format!(
            "{} todo(s) are waiting for review",
            needs_review_count
        ));
    }
    if task_failed_count > 0 {
        let suffix = if task_failed_still_blocks {
            " without a later resolution signal"
        } else {
            " (superseded by a later resolution signal)"
        };
        reasons.push(format!(
            "{} task_failed event(s) were observed{}",
            task_failed_count, suffix
        ));
    }
    if task_failed_still_blocks {
        reasons.push(
            "an unresolved failure requires human judgment before the project can continue"
                .to_string(),
        );
    }
    if follow_up_signal_count > 0 {
        reasons.push(format!(
            "{} follow-up/waiting signal(s) request more work",
            follow_up_signal_count
        ));
    }
    if ready_signal_count > 0 && explicit_pisci_handoff_count == 0 {
        reasons.push(format!(
            "{} ready-for-review signal(s) did not explicitly hand off to @pisci",
            ready_signal_count
        ));
    }
    if unfinished_work_count == 0
        && blocked_todo_count == 0
        && needs_review_count == 0
        && !task_failed_still_blocks
        && follow_up_signal_count == 0
        && explicit_pisci_handoff_count == 0
    {
        reasons.push(
            "worker-visible work appears exhausted; Pisci must make the next global decision"
                .to_string(),
        );
    }
    reasons
}

pub fn assess_project_state(
    messages: &[PoolMessage],
    todos: &[KoiTodo],
    _koi_ids: &[String],
) -> ProjectAssessment {
    if messages.is_empty() && todos.is_empty() {
        return ProjectAssessment {
            decision: ProjectDecision::Continue,
            active_todo_count: 0,
            blocked_todo_count: 0,
            needs_review_count: 0,
            task_failed_count: 0,
            follow_up_signal_count: 0,
            ready_signal_count: 0,
            explicit_pisci_handoff_count: 0,
            attention_reasons: Vec::new(),
            summary: "Project state is fully quiescent: no todos, no signals, and no observed coordination pressure.".into(),
        };
    }
    let active_todos: Vec<_> = todos
        .iter()
        .filter(|t| {
            matches!(
                t.status.as_str(),
                "todo" | "in_progress" | "blocked" | "needs_review"
            )
        })
        .collect();
    let unfinished_work_count = todos
        .iter()
        .filter(|t| matches!(t.status.as_str(), "todo" | "in_progress" | "blocked"))
        .count();
    let blocked_todo_count = active_todos
        .iter()
        .filter(|t| t.status == "blocked")
        .count();
    let needs_review_count = active_todos
        .iter()
        .filter(|t| t.status == "needs_review")
        .count();
    let active_todo_count = active_todos.len();

    let recent_task_failed_count = messages
        .iter()
        .filter(|m| m.event_type.as_deref() == Some("task_failed"))
        .count();
    let latest_task_failed_at: Option<DateTime<Utc>> = messages
        .iter()
        .filter(|m| m.event_type.as_deref() == Some("task_failed"))
        .map(|m| m.created_at)
        .max();
    let latest_needs_review_at: Option<DateTime<Utc>> = todos
        .iter()
        .filter(|t| t.status == "needs_review")
        .map(|t| t.updated_at)
        .max();

    let mut latest_signals: HashMap<String, SenderState> = HashMap::new();
    let mut latest_signal_times: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut ordered_messages: Vec<&PoolMessage> = messages.iter().collect();
    ordered_messages.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    for msg in &ordered_messages {
        if let Some(state) = extract_sender_state(msg) {
            latest_signals.insert(msg.sender_id.clone(), state);
            latest_signal_times.insert(msg.sender_id.clone(), msg.created_at);
        }
    }

    let follow_up_signal_count = latest_signals
        .values()
        .filter(|s| {
            matches!(
                s.signal,
                CoordinationSignalKind::FollowUpNeeded | CoordinationSignalKind::Waiting
            )
        })
        .count();
    let ready_states: Vec<_> = latest_signals
        .iter()
        .filter(|(_, s)| s.signal == CoordinationSignalKind::ReadyForPisciReview)
        .map(|(sender, state)| (sender.clone(), *state))
        .collect();
    let ready_signal_count = ready_states.len();
    let explicit_pisci_handoff_count = ready_states
        .iter()
        .filter(|(_, s)| s.mentions_pisci)
        .count();
    let latest_explicit_handoff_at: Option<DateTime<Utc>> = ready_states
        .iter()
        .filter(|(_, state)| state.mentions_pisci)
        .filter_map(|(sender, _)| latest_signal_times.get(sender).copied())
        .max();

    // Convergence guard: a historical task_failed only blocks convergence
    // when it is the most recent resolution signal. A later explicit
    // @pisci handoff or a fresh needs_review todo counts as the koi's
    // considered judgment that the failure has been addressed.
    let task_failed_still_blocks = match latest_task_failed_at {
        None => false,
        Some(failed_at) => {
            let handoff_after = latest_explicit_handoff_at
                .map(|h| h > failed_at)
                .unwrap_or(false);
            let review_after = latest_needs_review_at
                .map(|r| r > failed_at)
                .unwrap_or(false);
            !(handoff_after || review_after)
        }
    };

    let attention_reasons = build_attention_reasons(
        unfinished_work_count,
        blocked_todo_count,
        needs_review_count,
        recent_task_failed_count,
        task_failed_still_blocks,
        follow_up_signal_count,
        ready_signal_count,
        explicit_pisci_handoff_count,
    );

    if unfinished_work_count > 0 {
        let mut hints = Vec::new();
        if blocked_todo_count > 0 {
            hints.push(format!("{} blocked", blocked_todo_count));
        }
        if needs_review_count > 0 {
            hints.push(format!("{} needs_review", needs_review_count));
        }
        if recent_task_failed_count > 0 {
            hints.push(format!(
                "{} task_failed event(s) 鈥?a Koi may have timed out or crashed",
                recent_task_failed_count
            ));
        }
        let summary = if hints.is_empty() {
            format!(
                "Project state shows {} unfinished todo(s). More work is still in progress.",
                unfinished_work_count
            )
        } else {
            format!(
                "Project state shows {} unfinished todo(s) ({}). More work is still in progress.",
                unfinished_work_count,
                hints.join(", ")
            )
        };
        return ProjectAssessment {
            decision: ProjectDecision::Continue,
            active_todo_count,
            blocked_todo_count,
            needs_review_count,
            task_failed_count: recent_task_failed_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            attention_reasons,
            summary,
        };
    }

    if task_failed_still_blocks {
        return ProjectAssessment {
            decision: ProjectDecision::EscalateToHuman,
            active_todo_count,
            blocked_todo_count,
            needs_review_count,
            task_failed_count: recent_task_failed_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            attention_reasons,
            summary: format!(
                "Project state shows no unfinished todos, but {} task_failed event(s) still have no later resolution signal. This should be escalated to the user for a human decision.",
                recent_task_failed_count
            ),
        };
    }

    if needs_review_count > 0 {
        return ProjectAssessment {
            decision: ProjectDecision::ReadyForPisciReview,
            active_todo_count,
            blocked_todo_count,
            needs_review_count,
            task_failed_count: recent_task_failed_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            attention_reasons,
            summary: format!(
                "Project state shows {} todo(s) waiting for review and no unfinished implementation work.",
                needs_review_count
            ),
        };
    }

    if explicit_pisci_handoff_count > 0 {
        return ProjectAssessment {
            decision: ProjectDecision::ReadyForPisciReview,
            active_todo_count,
            blocked_todo_count,
            needs_review_count,
            task_failed_count: recent_task_failed_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            attention_reasons,
            summary: format!(
                "Project state shows {} ready-for-review handoff(s) to @pisci and no unfinished todos.",
                explicit_pisci_handoff_count
            ),
        };
    }

    if follow_up_signal_count > 0 {
        return ProjectAssessment {
            decision: ProjectDecision::Continue,
            active_todo_count,
            blocked_todo_count,
            needs_review_count,
            task_failed_count: recent_task_failed_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            attention_reasons,
            summary: format!(
                "Project state shows no unfinished todos, but {} sender(s) are still asking for follow-up or waiting.",
                follow_up_signal_count
            ),
        };
    }

    if ready_signal_count > 0 {
        return ProjectAssessment {
            decision: ProjectDecision::SupervisorDecisionRequired,
            active_todo_count,
            blocked_todo_count,
            needs_review_count,
            task_failed_count: recent_task_failed_count,
            follow_up_signal_count,
            ready_signal_count,
            explicit_pisci_handoff_count,
            attention_reasons,
            summary: format!(
                "Project state shows {} ready-for-review signal(s), but there is still no explicit ready-for-review handoff to @pisci. Pisci must inspect the pool and make the next global decision.",
                ready_signal_count
            ),
        };
    }

    ProjectAssessment {
        decision: ProjectDecision::SupervisorDecisionRequired,
        active_todo_count,
        blocked_todo_count,
        needs_review_count,
        task_failed_count: recent_task_failed_count,
        follow_up_signal_count,
        ready_signal_count,
        explicit_pisci_handoff_count,
        attention_reasons,
        summary:
            "Project state shows no unfinished todos, but there is still no explicit ready-for-review handoff to @pisci. Pisci must inspect the pool and decide what happens next."
                .into(),
    }
}
