pub mod heartbeat {
    use crate::models::{KoiTodo, PoolMessage, PoolSession};
    use crate::project_state::{
        assess_project_state, contains_pisci_mention, extract_project_status_signal,
        ProjectAssessment, ProjectDecision,
    };

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
            Some(
                "task_completed"
                    | "task_failed"
                    | "task_claimed"
                    | "task_blocked"
                    | "task_cancelled"
                    | "protocol_reminder"
                    | "task_progress"
            )
        )
    }

    pub fn pool_pisci_session_id(pool_id: &str) -> String {
        format!("pisci_pool_{}", pool_id)
    }

    pub fn build_pool_heartbeat_message(base_prompt: &str, attention: &PoolAttention) -> String {
        let assessment = &attention.assessment;
        let mut lines = vec![
            base_prompt.to_string(),
            String::new(),
            "## Heartbeat Inbox".to_string(),
            attention.summary.clone(),
            String::new(),
            "## Current Project State".to_string(),
            "- The fields below are a host-generated snapshot of observable state, not a final project judgment.".to_string(),
            format!("- Decision: {:?}", assessment.decision),
            format!("- Active todos: {}", assessment.active_todo_count),
            format!("- Blocked todos: {}", assessment.blocked_todo_count),
            format!("- Needs-review todos: {}", assessment.needs_review_count),
            format!("- task_failed events: {}", assessment.task_failed_count),
            format!("- Follow-up signals: {}", assessment.follow_up_signal_count),
            format!("- Assessment: {}", assessment.summary),
            String::new(),
            "## Attention Reasons".to_string(),
        ];
        if assessment.attention_reasons.is_empty() {
            lines.push("- No explicit attention reason was derived; inspect the latest pool state before acting.".to_string());
        } else {
            lines.extend(
                assessment
                    .attention_reasons
                    .iter()
                    .map(|reason| format!("- {}", reason)),
            );
        }
        lines.extend([String::new(), "## Guidance".to_string()]);

        match assessment.decision {
            ProjectDecision::Continue => {
                lines.push(
                    "The project snapshot still shows active coordination pressure. Inspect pool_chat, the task board, and the org_spec before deciding whether Pisci should act now or continue waiting."
                        .to_string(),
                );
                if assessment.active_todo_count == 0
                    && (assessment.follow_up_signal_count > 0 || assessment.task_failed_count > 0)
                {
                    lines.push(
                        "There is coordination pressure without a clear active owner. Treat this as an attention gap to investigate, not as permission to pick a specific next actor automatically."
                            .to_string(),
                    );
                }
                lines.push(
                    "Do not assume a fixed reviewer or handoff target. Use the current pool evidence and org_spec to decide the smallest effective coordination action."
                        .to_string(),
                );
            }
            ProjectDecision::SupervisorDecisionRequired => {
                lines.push(
                    "The worker agents appear locally finished, but no worker can make the final global judgment for the project."
                        .to_string(),
                );
                lines.push(
                    "Pisci must now inspect the pool evidence and decide the next step explicitly: coordinate more work, request clarification, or treat the project as ready for Pisci's own review."
                        .to_string(),
                );
                lines.push(
                    "Do NOT collapse this state into a fixed canned outcome. Use the org_spec, recent pool_chat, and deliverables to decide whether the project truly converged or whether the task decomposition missed something."
                        .to_string(),
                );
                lines.push(
                    "If you decide the user should weigh in before the project moves forward, call `app_control` with action='notify_user' (level='warning' or 'info', include pool_id) to surface a toast in the main UI. Do this only when user input genuinely helps, not as a routine status update."
                        .to_string(),
                );
            }
            ProjectDecision::EscalateToHuman => {
                lines.push(
                    "The project reached a state that should NOT be resolved by further autonomous retries or automatic routing."
                        .to_string(),
                );
                lines.push(
                    "Treat this as a human-escalation state: inspect the failure evidence, summarize what became impossible or unsafe to decide automatically, and stop short of inventing a new worker plan unless the user explicitly approves it."
                        .to_string(),
                );
                lines.push(
                    "Your role here is to surface the situation clearly for the user, not to silently convert an unrecoverable failure into a normal project continuation."
                        .to_string(),
                );
                lines.push(
                    "Raise a user-visible toast via `app_control` with action='notify_user', level='critical', duration_ms=0, and include pool_id plus a 1-2 sentence summary of why human judgment is needed. The system may have auto-posted a baseline toast already; your call adds the diagnostic summary."
                        .to_string(),
                );
            }
            ProjectDecision::ReadyForPisciReview => {
                lines.push(
                    "The snapshot is compatible with Pisci review, but HEARTBEAT_OK is never automatic."
                        .to_string(),
                );
                lines.push(
                    "Suggested actions: read pool_chat to confirm completion, merge branches if applicable, post a summary, and leave the pool active until the user explicitly asks to archive it. HEARTBEAT_OK is still not automatic. Do NOT archive the project during heartbeat. Reply HEARTBEAT_OK only when satisfied."
                        .to_string(),
                );
                lines.push("If you discover unresolved work, keep the project open and coordinate the next step explicitly.".to_string());
            }
        }

        lines.push(String::new());
        lines.push(
            "Use your judgment. Read the pool context, then take whatever action best serves the project."
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
        let has_historic_pisci_route = messages
            .iter()
            .any(|msg| contains_pisci_mention(&msg.content));

        let has_state_attention = !assessment.attention_reasons.is_empty();
        if new_attention_messages.is_empty()
            && assessment.decision == ProjectDecision::EscalateToHuman
            && has_historic_pisci_route
        {
            return None;
        }
        if new_attention_messages.is_empty()
            && assessment.decision == ProjectDecision::SupervisorDecisionRequired
            && has_historic_pisci_route
        {
            return None;
        }
        if new_attention_messages.is_empty()
            && assessment.decision == ProjectDecision::Continue
            && !has_state_attention
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
            session_id: pool_pisci_session_id(&pool.id),
            summary: lines.join("\n"),
            assessment,
        })
    }
}

pub mod models {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct KoiTodo {
        pub id: String,
        pub owner_id: String,
        pub title: String,
        pub description: String,
        pub status: String,
        pub priority: String,
        pub assigned_by: String,
        pub pool_session_id: Option<String>,
        pub claimed_by: Option<String>,
        pub claimed_at: Option<DateTime<Utc>>,
        pub depends_on: Option<String>,
        pub blocked_reason: Option<String>,
        pub result_message_id: Option<i64>,
        pub source_type: String,
        #[serde(default)]
        pub task_timeout_secs: u32,
        pub created_at: DateTime<Utc>,
        pub updated_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PoolSession {
        pub id: String,
        pub name: String,
        pub org_spec: String,
        pub status: String,
        pub project_dir: Option<String>,
        #[serde(default)]
        pub task_timeout_secs: u32,
        pub last_active_at: Option<DateTime<Utc>>,
        pub created_at: DateTime<Utc>,
        pub updated_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PoolMessage {
        pub id: i64,
        pub pool_session_id: String,
        pub sender_id: String,
        pub content: String,
        pub msg_type: String,
        pub metadata: String,
        pub todo_id: Option<String>,
        pub reply_to_message_id: Option<i64>,
        pub event_type: Option<String>,
        pub created_at: DateTime<Utc>,
    }
}

pub mod project_state {
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
        let blocked_todo_count = active_todos.iter().filter(|t| t.status == "blocked").count();
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
}

pub mod scene {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SceneKind {
        MainChat,
        PoolCoordinator,
        KoiTask,
        IMHeadless,
        HeartbeatSupervisor,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RegistryProfile {
        MainChat,
        PoolCoordinator,
        KoiTask,
        IMHeadless,
        HeartbeatSupervisor,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CollaborationContextMode {
        Never,
        OnDemand,
        Required,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HistorySliceMode {
        FullRecent,
        SummaryOnly,
        None,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EventDigestMode {
        Off,
        CoordinationOnly,
        CoordinationPlusFailures,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MemorySliceMode {
        Off,
        ScopedSearch,
        ScopedPlusRecent,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PoolSnapshotMode {
        Off,
        Compact,
        Full,
    }

    const POOL_COORDINATOR_TOOLS: &[&str] = &[
        "file_read",
        "file_write",
        "file_edit",
        "file_diff",
        "code_run",
        "file_search",
        "file_list",
        "shell",
        "web_search",
        "browser",
        "memory_store",
        "call_fish",
        "pool_org",
        "pool_chat",
        "vision_context",
        "skill_list",
        "ssh",
        "pdf",
    ];

    const KOI_TASK_TOOLS: &[&str] = &[
        "file_read",
        "file_write",
        "file_edit",
        "file_diff",
        "code_run",
        "file_search",
        "file_list",
        "shell",
        "web_search",
        "browser",
        "memory_store",
        "call_fish",
        "call_koi",
        "pool_org",
        "pool_chat",
        "vision_context",
        "skill_list",
        "ssh",
        "pdf",
    ];

    const HEARTBEAT_SUPERVISOR_TOOLS: &[&str] = &[
        "file_read",
        "file_write",
        "file_edit",
        "file_diff",
        "code_run",
        "file_search",
        "file_list",
        "shell",
        "web_search",
        "browser",
        "pool_org",
        "pool_chat",
        "vision_context",
        "ssh",
        "pdf",
    ];

    #[derive(Debug, Clone, Copy)]
    pub struct ScenePolicy {
        pub registry_profile: RegistryProfile,
        pub allow_skill_loader: bool,
        pub include_memory: bool,
        pub include_task_state: bool,
        pub include_pool_roster: bool,
        pub include_pool_context: bool,
        pub include_project_instructions: bool,
        pub injection_budget_ratio: f64,
        pub injection_budget_min_chars: usize,
        pub auto_compact_threshold_override: Option<u32>,
    }

    fn compute_total_input_budget(context_window: u32, max_tokens: u32) -> usize {
        let window = if context_window > 0 {
            context_window as usize
        } else {
            match max_tokens {
                t if t >= 8192 => 128_000,
                t if t >= 4096 => 64_000,
                _ => 32_000,
            }
        };
        let usable = window.saturating_sub(max_tokens as usize);
        (usable as f64 * 0.85) as usize
    }

    impl ScenePolicy {
        pub fn for_kind(kind: SceneKind) -> Self {
            match kind {
                SceneKind::MainChat => Self {
                    registry_profile: RegistryProfile::MainChat,
                    allow_skill_loader: true,
                    include_memory: true,
                    include_task_state: true,
                    include_pool_roster: true,
                    include_pool_context: false,
                    include_project_instructions: true,
                    injection_budget_ratio: 0.15,
                    injection_budget_min_chars: 2_000,
                    auto_compact_threshold_override: None,
                },
                SceneKind::PoolCoordinator => Self {
                    registry_profile: RegistryProfile::PoolCoordinator,
                    allow_skill_loader: false,
                    include_memory: true,
                    include_task_state: true,
                    include_pool_roster: false,
                    include_pool_context: true,
                    include_project_instructions: false,
                    injection_budget_ratio: 0.10,
                    injection_budget_min_chars: 1_500,
                    auto_compact_threshold_override: Some(0),
                },
                SceneKind::KoiTask => Self {
                    registry_profile: RegistryProfile::KoiTask,
                    allow_skill_loader: true,
                    include_memory: true,
                    include_task_state: false,
                    include_pool_roster: false,
                    include_pool_context: true,
                    include_project_instructions: false,
                    injection_budget_ratio: 0.10,
                    injection_budget_min_chars: 1_500,
                    auto_compact_threshold_override: Some(0),
                },
                SceneKind::IMHeadless => Self {
                    registry_profile: RegistryProfile::IMHeadless,
                    allow_skill_loader: false,
                    include_memory: true,
                    include_task_state: true,
                    include_pool_roster: false,
                    include_pool_context: false,
                    include_project_instructions: false,
                    injection_budget_ratio: 0.10,
                    injection_budget_min_chars: 1_500,
                    auto_compact_threshold_override: Some(0),
                },
                SceneKind::HeartbeatSupervisor => Self {
                    registry_profile: RegistryProfile::HeartbeatSupervisor,
                    allow_skill_loader: false,
                    include_memory: false,
                    include_task_state: false,
                    include_pool_roster: false,
                    include_pool_context: true,
                    include_project_instructions: false,
                    injection_budget_ratio: 0.06,
                    injection_budget_min_chars: 1_200,
                    auto_compact_threshold_override: Some(0),
                },
            }
        }

        pub fn compute_injection_budget(self, context_window: u32, max_tokens: u32) -> usize {
            let total_budget = compute_total_input_budget(context_window, max_tokens);
            ((total_budget as f64 * self.injection_budget_ratio) as usize * 4)
                .max(self.injection_budget_min_chars)
        }

        pub fn effective_auto_compact_threshold(self, configured: u32) -> u32 {
            self.auto_compact_threshold_override.unwrap_or(configured)
        }

        pub fn project_instructions_enabled(self, configured: bool) -> bool {
            self.include_project_instructions && configured
        }

        pub fn collaboration_context_mode(self) -> CollaborationContextMode {
            match self.registry_profile {
                RegistryProfile::MainChat => CollaborationContextMode::OnDemand,
                RegistryProfile::IMHeadless => CollaborationContextMode::Never,
                RegistryProfile::PoolCoordinator
                | RegistryProfile::KoiTask
                | RegistryProfile::HeartbeatSupervisor => CollaborationContextMode::Required,
            }
        }

        pub fn tool_allowlist(self) -> Option<&'static [&'static str]> {
            match self.registry_profile {
                RegistryProfile::MainChat | RegistryProfile::IMHeadless => None,
                RegistryProfile::PoolCoordinator => Some(POOL_COORDINATOR_TOOLS),
                RegistryProfile::KoiTask => Some(KOI_TASK_TOOLS),
                RegistryProfile::HeartbeatSupervisor => Some(HEARTBEAT_SUPERVISOR_TOOLS),
            }
        }

        pub fn history_slice_mode(self) -> HistorySliceMode {
            match self.registry_profile {
                RegistryProfile::MainChat | RegistryProfile::IMHeadless => {
                    HistorySliceMode::FullRecent
                }
                RegistryProfile::PoolCoordinator | RegistryProfile::HeartbeatSupervisor => {
                    HistorySliceMode::SummaryOnly
                }
                RegistryProfile::KoiTask => HistorySliceMode::None,
            }
        }

        pub fn event_digest_mode(self) -> EventDigestMode {
            match self.registry_profile {
                RegistryProfile::MainChat | RegistryProfile::IMHeadless => EventDigestMode::Off,
                RegistryProfile::PoolCoordinator => EventDigestMode::CoordinationPlusFailures,
                RegistryProfile::KoiTask => EventDigestMode::CoordinationPlusFailures,
                RegistryProfile::HeartbeatSupervisor => EventDigestMode::CoordinationPlusFailures,
            }
        }

        pub fn memory_slice_mode(self) -> MemorySliceMode {
            if !self.include_memory {
                return MemorySliceMode::Off;
            }
            match self.registry_profile {
                RegistryProfile::KoiTask => MemorySliceMode::ScopedPlusRecent,
                RegistryProfile::MainChat
                | RegistryProfile::PoolCoordinator
                | RegistryProfile::IMHeadless
                | RegistryProfile::HeartbeatSupervisor => MemorySliceMode::ScopedSearch,
            }
        }

        pub fn pool_snapshot_mode(self) -> PoolSnapshotMode {
            match self.registry_profile {
                RegistryProfile::IMHeadless => PoolSnapshotMode::Off,
                RegistryProfile::KoiTask
                | RegistryProfile::PoolCoordinator
                | RegistryProfile::HeartbeatSupervisor => PoolSnapshotMode::Compact,
                RegistryProfile::MainChat => PoolSnapshotMode::Full,
            }
        }

        pub fn recent_pool_message_limit(self) -> usize {
            match self.registry_profile {
                RegistryProfile::MainChat => 8,
                RegistryProfile::PoolCoordinator => 10,
                RegistryProfile::KoiTask => 12,
                RegistryProfile::IMHeadless => 0,
                RegistryProfile::HeartbeatSupervisor => 6,
            }
        }

        pub fn recent_pool_message_chars(self) -> usize {
            match self.registry_profile {
                RegistryProfile::MainChat => 180,
                RegistryProfile::PoolCoordinator => 220,
                RegistryProfile::KoiTask => 260,
                RegistryProfile::IMHeadless => 0,
                RegistryProfile::HeartbeatSupervisor => 180,
            }
        }

        pub fn org_spec_preview_chars(self) -> usize {
            match self.registry_profile {
                RegistryProfile::MainChat => 600,
                RegistryProfile::PoolCoordinator => 900,
                RegistryProfile::KoiTask => 1_200,
                RegistryProfile::IMHeadless => 0,
                RegistryProfile::HeartbeatSupervisor => 600,
            }
        }
    }
}

pub mod trial {
    pub fn effective_trial_koi_status(db_status: &str, run_slot_active: bool) -> &str {
        if run_slot_active { "busy" } else { db_status }
    }
}

/// Koi's shared prompt contract.
///
/// The Koi system prompt is assembled as a fixed **6-layer structure**. Five of
/// the layers live in this module as pure `&'static str` helpers; the 6th layer
/// (Identity) is provided by the calling site as the Koi's own system prompt
/// plus the `"You are <name>"` preamble. The layer order is load-bearing: the
/// **Stop Gate** must always be the last section the model sees before it acts.
///
/// Layer map:
///   Layer 1 路 Identity          鈥?provided by caller (koi_system_prompt + name)
///   Layer 2 路 Run Shape         鈥?`koi_run_shape_prompt`
///   Layer 3 路 Coordination      鈥?`koi_coordination_protocol_prompt`
///   Layer 4 路 Context & Tools   鈥?`koi_context_and_tools_prompt`
///   Layer 5 路 Optional Caps     鈥?`koi_capabilities_prompt`
///   Layer 6 路 Stop Gate (LAST)  鈥?`koi_stop_gate_prompt`
///
/// Anything a Koi must do on EVERY run belongs in Run Shape (Layer 2) or Stop
/// Gate (Layer 6). These are shared protocol invariants 鈥?role-specific
/// behaviour must NOT be hardcoded here.
pub mod koi_prompt {
    pub fn koi_run_shape_prompt() -> &'static str {
        "## Run Shape\n\
Every run follows exactly ONE of three trajectories. Pick the trajectory at the start of the run and execute its phases in order. Trajectory choice is determined by what the pool actually needs from you right now, not by what was historically expected of your role.\n\
\n\
### Observer trajectory\n\
Use this when nothing in the pool is actionable for you right now.\n\
- Read pool_chat (and pool_org if relevant) to confirm.\n\
- Do NOT claim any todo. Do NOT call any tool that changes shared state \u{2014} no file_write, no file_edit, no shell that mutates state, no pool_chat post.\n\
- Stop.\n\
\n\
### Actor trajectory\n\
Use this when concrete actionable work has been handed to you. The trajectory has FOUR phases and you cannot exit between them.\n\
1. **Setup.** Make sure the work appears on the board. If no suitable todo exists, `create_todo`. Then `claim_todo`. After claim succeeds you are in the Acting phase.\n\
2. **Acting.** Produce the deliverable using whatever tools fit (file_write, code_run, shell, browser, file_read, analysis in your reasoning, etc.). The Acting phase ends the moment the deliverable exists in any concrete form.\n\
3. **Reconciling.** Mandatory after Acting and the most commonly skipped phase. Before the run may end you MUST complete ALL of:\n\
   a. Post a pool_chat message that makes the deliverable observable to the rest of the team. For file outputs include the path(s) and a brief summary; for non-file outputs (analysis, decision, spec) include the content directly in the post.\n\
   b. If continuation by another agent is needed, identify that agent from the project's `org_spec`, your task description, or recent pool_chat history \u{2014} do NOT default to a fixed role and do NOT assume a `Reviewer`/`Coder`/`Architect` exists. If you cannot confidently identify the next actor, state that explicitly in pool_chat and let Pisci route. When the next actor is identified, pair the deliverable post with `[ProjectStatus] follow_up_needed` and an `@mention` of that agent.\n\
   c. If no continuation is needed and the project may be ready to close, post `[ProjectStatus] ready_for_pisci_review @pisci`. Do NOT @mention peer agents to confirm completion.\n\
   d. Call `pool_org(action=\"complete_todo\", todo_id=..., summary=...)` on the todo you claimed in Setup. `complete_todo` is the wire signal that moves the run from Reconciling toward Done; nothing else replaces it \u{2014} not a chat post, not a successful test, not your reasoning that the work is done.\n\
4. **Done.** Only after Reconciling steps a, b/c, and d have all completed may you stop.\n\
\n\
### Waiter trajectory\n\
Use this when you entered Setup or Acting but discovered the work cannot proceed (real blocker, missing upstream evidence, work no longer needed).\n\
- Set the claimed todo to `blocked` (with a specific reason another agent can act on) or call `cancel_todo` (with reason).\n\
- Post a pool_chat message naming the waiting condition.\n\
- Stop.\n\
\n\
### Hard invariants (re-read every run)\n\
- **The board is the source of truth for run state, not your narrative.** A run is incomplete as long as any todo you claimed in this run still has status `todo` or `in_progress`. You cannot text-summarize your way past that fact.\n\
- **Production is not termination.** A deliverable that exists only in your worktree, your message text, or your reasoning is invisible to the team. The run only reaches Done after the deliverable is observable from pool_chat AND the corresponding todo has been reconciled on the board via `complete_todo` (or `blocked`/`cancel_todo`).\n\
- **The runtime safety net is visible.** If you exit the run with a claimed todo still in `in_progress`, the runtime will rewrite that todo to `needs_review` and post a `protocol_reminder` event in pool_chat under your name. That event is permanent and visible to every agent that subsequently joins the pool. Treat triggering it as a logged failure, not a free recovery path.\n"
    }

    pub fn koi_coordination_protocol_prompt() -> &'static str {
        "## Coordination Protocol\n\
pool_chat is the shared channel; pool_org is the shared task board. These are the only load-bearing surfaces \u{2014} coordination that is not visible here does not exist for other agents.\n\
- `pool_chat(action=\"read\")` to see history; `pool_chat(action=\"send\")` to post. `pool_org(action=\"get_todos\")` to see the board.\n\
- @mention another agent ONLY when you are genuinely handing them concrete actionable work. Do NOT @mention peers just to acknowledge, agree, thank, or declare the project done \u{2014} those messages create noise and can trigger reply loops.\n\
- **Handoff messages must propagate the protocol, not just the task.** When you @mention another agent to hand off work, your message MUST include three things, not one: (1) WHAT to do (the deliverable you expect), (2) WHERE the inputs are (file path, spec link, prior message reference), and (3) HOW to report completion \u{2014} name the expected next reporting target (return to you, hand to a third party identified from `org_spec`, or signal `@pisci`) and the `[ProjectStatus]` signal expected at completion. A handoff that says only \"do X\" silently transfers the cognitive load of figuring out completion semantics to the receiver, and receivers commonly drop the protocol when their attention is consumed by production. Treat your handoff message as the receiver's task brief.\n\
- Identify the next responsible party from project context, not from a fixed role catalogue. Inputs in priority order: (1) the project's `org_spec` (which agent owns this kind of work), (2) the latest task description in pool_chat, (3) the most recent @mention chain. If multiple inputs disagree, prefer org_spec. If no input identifies the next party with confidence, do NOT guess and do NOT default to any role name \u{2014} state the ambiguity in pool_chat and let Pisci route.\n\
- Not every @mention of your name is a live handoff. If your name appears inside a future plan, a conditional (\"after X is done, ask @you\"), or a status recap, it is not work for you right now. Decide actionability from the latest pool evidence.\n\
- Status signals (place verbatim inside your pool_chat message so Pisci can reason about project state):\n\
  - `[ProjectStatus] follow_up_needed` \u{2014} more work is required; pair with an @mention of the next responsible party identified per the rule above.\n\
  - `[ProjectStatus] waiting` \u{2014} you are blocked on something specific; name what you are waiting on.\n\
  - `[ProjectStatus] ready_for_pisci_review` \u{2014} use ONLY after your own `complete_todo` has succeeded and the project looks ready for Pisci to close.\n\
- Never unilaterally declare the project complete. If you believe the project may be done, signal `@pisci`; do not poll peer agents for agreement.\n\
- Only Pisci or the user directly assigns work to you. Other agents request via @mention. The task board (pool_org) and chat (pool_chat) are your sources of truth; do not rely on heartbeat, trial, or other harnesses to repair missing coordination.\n"
    }

    pub fn koi_context_and_tools_prompt() -> &'static str {
        "## Context And Tools\n\
- The task itself and the latest relevant pool_chat messages are your primary working context. Start from them before reaching for broader tools.\n\
- Use external tools only to close a specific, named gap in the current deliverable. If you cannot name the exact file, path, or artifact you need, do NOT call file or search tools yet.\n\
- If the task is primarily discussion, analysis, review, specification, or status \u{2014} answer directly from the task and pool context; do not fabricate tool detours.\n\
- Do not narrate intended future actions as your result. The deliverable must be observable (posted to pool_chat, written to a file, recorded as a todo transition) \u{2014} not merely described.\n\
- Worktree discipline: if you are in a Git worktree, your [Environment] workspace IS your worktree directory (e.g. `.../.koi-worktrees/<name>-<short-id>`). Use RELATIVE paths for every file operation. Writing to absolute paths into the main project directory will corrupt the shared codebase.\n\
- Your changes are auto-committed when the run ends; do NOT run `git add`, `git commit`, `git merge`, `git rebase`, or `git push` yourself \u{2014} branch integration is Pisci's responsibility. When your code work is done, note in pool_chat which branch is ready to merge.\n\
- If your task depends on another Koi's code, ask in pool_chat which branch it lives on so Pisci can merge it first. Stay inside your assigned scope; do not modify files outside the directories relevant to your task.\n\
- Long output rule: if your deliverable is longer than ~500 words, write the full content to a file and post only a brief summary plus the exact file path in pool_chat. When delegating via call_koi, pass the file path, not the full content.\n\
- Knowledge base: the workspace contains a shared `kb/` directory that persists across runs. Consult it when prior project memory is likely to matter. Write durable notes as `.md`; write structured records as `.jsonl` with `timestamp`, `author`, and `summary`.\n"
    }

    pub fn koi_capabilities_prompt() -> &'static str {
        "## Optional Capabilities\n\
- Skills: call `skill_list` only when a skill is likely to materially help. If a matching skill exists, `file_read` its SKILL.md and follow it as a method in service of the actual task \u{2014} skill discovery does not replace execution.\n\
- Sub-task delegation (call_fish): Fish are stateless, ephemeral workers. Use call_fish only for tasks with many mechanical intermediate steps whose details are not relevant to the final answer. Do not use call_fish for work that requires your own judgment, sustained iteration, or a single simple action. Always `call_fish(action=\"list\")` first, and write a complete self-contained task description.\n\
- Memory: when you learn something project-relevant that is worth persisting beyond this run, call memory_store. Scope it correctly \u{2014} private to you vs. shared with the pool.\n"
    }

    pub fn koi_stop_gate_prompt() -> &'static str {
        "## Stop Gate \u{2014} board state check, immediately before exit\n\
This is the LAST thing you read. Treat it as a state check on the board, not as a self-narrative checklist. Re-perform it once per run, every run, without exception.\n\
\n\
1. **Board check (unconditional).** Call `pool_org(action=\"get_todos\")` and look at the todos you claimed in THIS run. Any todo of yours still in status `todo` or `in_progress` means the run is not finished \u{2014} you are still in the Acting or Reconciling phase from Run Shape. You may NOT exit while that is true. Decide which phase to return to:\n\
   - If the deliverable does not yet exist in concrete form \u{2014} return to Acting and finish it.\n\
   - If the deliverable exists but you have not yet posted it to pool_chat or called `complete_todo` \u{2014} return to Reconciling and complete steps a, b/c, and d.\n\
   - If the work cannot proceed \u{2014} switch to the Waiter trajectory: set the todo to `blocked` or call `cancel_todo` with a reason another agent can act on, post the waiting condition to pool_chat, then exit.\n\
\n\
2. **Visibility check.** If this run produced any deliverable, confirm by reading the latest pool_chat that the deliverable is observable there (content posted directly, or file path(s) plus a brief summary). \"I will summarize next run\" is not allowed \u{2014} the team cannot see your future runs. If it is not visible, post it now BEFORE calling `complete_todo`.\n\
\n\
3. **Continuation check.** If your output requires another specific agent to continue, confirm a `[ProjectStatus] follow_up_needed` post with that agent's `@mention` exists from THIS run. Identify the next responsible party from `org_spec` / task description / pool_chat history per the Coordination Protocol \u{2014} do NOT default to a role name. If your output looks like a project-ready conclusion, confirm `[ProjectStatus] ready_for_pisci_review @pisci` exists from THIS run AND was posted only after your `complete_todo` succeeded. Do NOT @mention peer agents for agreement.\n\
\n\
**Exit is permitted only when (1) is unambiguously \"no claimed todo of mine is in todo or in_progress\" AND (2) and (3) are satisfied as applicable.** The runtime enforces (1) for you: if you exit early, the runtime rewrites the stuck todo to `needs_review` and posts a `protocol_reminder` event in pool_chat under your name. That trace is permanent and visible to every agent that subsequently joins the pool \u{2014} it is a logged failure, not a redo.\n\
\n\
Anti-pattern reminder: passing tests, writing files, drafting a spec, or believing the work is done does NOT complete the run. The run completes only when (a) `complete_todo` (or `blocked` / `cancel_todo`) has succeeded on every claimed todo, (b) the deliverable is visible in pool_chat, and (c) any required handoff has already been posted with the correct `[ProjectStatus]` signal.\n"
    }

    /// Assemble the full Koi system prompt in the locked 6-layer order.
    /// The caller supplies the identity preamble and any dynamic context
    /// slices (continuity / memory / org_spec / pool_chat / assignment);
    /// this function appends the five fixed protocol sections with the
    /// Stop Gate as the final section.
    #[allow(clippy::too_many_arguments)]
    pub fn build_koi_task_system_prompt(
        koi_system_prompt: &str,
        koi_name: &str,
        koi_icon: &str,
        continuity_ctx: &str,
        memory_context: &str,
        org_spec_ctx: &str,
        pool_chat_ctx: &str,
        assignment_ctx: &str,
    ) -> String {
        format!(
            "{}\n\nYou are {} ({}). You are running in the KoiTask scene with your own independent memory and tool access. When you learn something important, use memory_store to save it.{}{}{}{}{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
            koi_system_prompt,
            koi_name,
            koi_icon,
            continuity_ctx,
            memory_context,
            org_spec_ctx,
            pool_chat_ctx,
            assignment_ctx,
            koi_run_shape_prompt(),
            koi_coordination_protocol_prompt(),
            koi_context_and_tools_prompt(),
            koi_capabilities_prompt(),
            koi_stop_gate_prompt(),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample_prompt() -> String {
            build_koi_task_system_prompt(
                "You are a helpful Koi.",
                "Alice",
                "(fish)",
                "",
                "",
                "",
                "",
                "",
            )
        }

        /// The Stop Gate must be the FINAL top-level section of the assembled
        /// prompt. If a later section were appended after it, the model would
        /// read the Stop Gate checklist and then be steered elsewhere before
        /// acting 鈥?defeating the whole point of the gate.
        #[test]
        fn system_prompt_ends_with_stop_gate_as_last_section() {
            let prompt = sample_prompt();
            let gate_idx = prompt
                .find("## Stop Gate")
                .expect("Stop Gate section must exist in the system prompt");
            let after_gate = &prompt[gate_idx + "## Stop Gate".len()..];
            assert!(
                !after_gate.contains("\n## "),
                "Stop Gate must be the final top-level section; found another '## ' header after it"
            );
        }

        /// Load-bearing protocol words that the Stop Gate must keep literally.
        /// The Stop Gate is a BOARD STATE CHECK \u{2014} it must reference the
        /// actual board API (pool_org / get_todos), the failure statuses that
        /// gate exit (todo / in_progress), the terminal reconciliation calls
        /// (complete_todo / cancel_todo / blocked), the project-completion
        /// signal (@pisci / ready_for_pisci_review), and the runtime safety
        /// net trace (protocol_reminder / needs_review). Losing any of these
        /// silently weakens convergence.
        #[test]
        fn stop_gate_contains_required_protocol_invariants() {
            let prompt = sample_prompt();
            let gate_idx = prompt.find("## Stop Gate").expect("stop gate section");
            let gate = &prompt[gate_idx..];
            for required in [
                "pool_org",
                "get_todos",
                "in_progress",
                "complete_todo",
                "cancel_todo",
                "blocked",
                "@pisci",
                "ready_for_pisci_review",
                "protocol_reminder",
                "needs_review",
            ] {
                assert!(
                    gate.contains(required),
                    "Stop Gate lost required invariant literal: {}",
                    required
                );
            }
        }

        /// The Actor trajectory must be expressed as an explicit four-phase
        /// state machine (Setup \u2192 Acting \u2192 Reconciling \u2192 Done),
        /// because the failure mode we are fighting is models conflating
        /// "deliverable produced" with "run finished" and skipping the
        /// Reconciling phase. If the trajectory collapses back to a flat
        /// list, silent-coder endings come back.
        #[test]
        fn run_shape_defines_trajectories_with_explicit_phases() {
            let prompt = sample_prompt();
            let shape_idx = prompt.find("## Run Shape").expect("Run Shape section");
            let coord_idx = prompt
                .find("## Coordination Protocol")
                .expect("Coordination Protocol section");
            let shape = &prompt[shape_idx..coord_idx];

            for label in ["Observer", "Actor", "Waiter"] {
                assert!(
                    shape.contains(label),
                    "Run Shape must define '{}' trajectory",
                    label
                );
            }
            for phase in ["Setup", "Acting", "Reconciling", "Done"] {
                assert!(
                    shape.contains(phase),
                    "Run Shape must name the '{}' phase of the Actor trajectory",
                    phase
                );
            }
            assert!(
                shape.contains("Production is not termination"),
                "Run Shape must keep the 'Production is not termination' invariant"
            );
            assert!(
                shape.contains("source of truth"),
                "Run Shape must keep the 'board is the source of truth' invariant"
            );
            assert!(
                shape.contains("protocol_reminder"),
                "Run Shape must surface the runtime safety net (protocol_reminder) so the agent knows it leaves a permanent trace"
            );
        }

        /// When a Koi hands off work to another agent, the handoff message
        /// must propagate the protocol \u2014 not just the task description.
        /// Otherwise the receiver's `task_input` ends up missing the
        /// "how to report completion" signal that keeps the chain converging.
        /// The Coordination Protocol section is where this requirement lives.
        #[test]
        fn coordination_protocol_requires_handoff_to_propagate_protocol() {
            let prompt = sample_prompt();
            let coord_idx = prompt
                .find("## Coordination Protocol")
                .expect("Coordination Protocol section");
            let ctx_idx = prompt
                .find("## Context And Tools")
                .expect("Context And Tools section");
            let coord = &prompt[coord_idx..ctx_idx];
            // The propagation requirement must be explicit, not implicit.
            assert!(
                coord.contains("Handoff messages must propagate the protocol"),
                "Coordination Protocol must explicitly require handoff to propagate protocol semantics"
            );
            // It must spell out the three required pieces a handoff carries.
            for piece in ["WHAT to do", "WHERE the inputs are", "HOW to report completion"] {
                assert!(
                    coord.contains(piece),
                    "Handoff propagation rule must enumerate '{}' as a required piece",
                    piece
                );
            }
        }

        /// The universal Koi prompt must NOT bake in project-specific role
        /// names. The next responsible party is identified at runtime from
        /// org_spec / task description / pool history. If a role name like
        /// "Reviewer" or "Coder" leaks into the universal prompt, the agent
        /// will hardcode that handoff target and break on projects that do
        /// not have such a role.
        #[test]
        fn universal_prompt_does_not_hardcode_project_specific_roles() {
            let prompt = sample_prompt();
            for forbidden in [
                "@Reviewer",
                "@Coder",
                "@Architect",
                "the Reviewer",
                "the Coder",
                "the Architect",
            ] {
                assert!(
                    !prompt.contains(forbidden),
                    "Universal Koi prompt must not hardcode project-specific role mention '{}'",
                    forbidden
                );
            }
        }

        /// The 6-layer structure (Identity + 5 named sections) must appear in
        /// the locked order. This is the contract between prompt-design docs
        /// and runtime behaviour.
        #[test]
        fn system_prompt_preserves_six_layer_order() {
            let prompt = sample_prompt();
            let expected_order = [
                "You are Alice",
                "## Run Shape",
                "## Coordination Protocol",
                "## Context And Tools",
                "## Optional Capabilities",
                "## Stop Gate",
            ];
            let mut cursor = 0usize;
            for marker in expected_order {
                match prompt[cursor..].find(marker) {
                    Some(rel) => cursor += rel + marker.len(),
                    None => panic!(
                        "Expected marker '{}' not found after position {} \u{2014} layer order is broken",
                        marker, cursor
                    ),
                }
            }
        }
    }
}
