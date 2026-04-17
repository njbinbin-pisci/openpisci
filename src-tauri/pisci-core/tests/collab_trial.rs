use chrono::{DateTime, Duration, TimeZone, Utc};
use pisci_core::heartbeat::{PoolAttention, build_pool_heartbeat_message, collect_pool_attention};
use pisci_core::models::{KoiTodo, PoolMessage, PoolSession};
use pisci_core::project_state::{
    CoordinationSignalKind, ProjectAssessment, ProjectDecision, STATUS_READY,
    assess_project_state, build_coordination_event_digest,
    coordination_event_type_for_content, enrich_pool_message_metadata,
};
use pisci_core::scene::{
    CollaborationContextMode, EventDigestMode, HistorySliceMode, MemorySliceMode,
    PoolSnapshotMode, RegistryProfile, SceneKind, ScenePolicy,
};
use pisci_core::trial::effective_trial_koi_status;
use serde_json::json;

fn sample_message(
    id: i64,
    sender_id: &str,
    content: &str,
    metadata: serde_json::Value,
    event_type: Option<&str>,
) -> PoolMessage {
    PoolMessage {
        id,
        pool_session_id: "pool-1".into(),
        sender_id: sender_id.into(),
        content: content.into(),
        msg_type: "text".into(),
        metadata: metadata.to_string(),
        todo_id: None,
        reply_to_message_id: None,
        event_type: event_type.map(str::to_string),
        created_at: Utc::now(),
    }
}

fn sample_todo(status: &str) -> KoiTodo {
    let now = Utc::now();
    KoiTodo {
        id: "todo-1".into(),
        owner_id: "koi-1".into(),
        title: "Investigate".into(),
        description: "Check project state".into(),
        status: status.into(),
        priority: "medium".into(),
        assigned_by: "pisci".into(),
        pool_session_id: Some("pool-1".into()),
        claimed_by: Some("koi-1".into()),
        claimed_at: Some(now),
        depends_on: None,
        blocked_reason: None,
        result_message_id: None,
        source_type: "pisci".into(),
        task_timeout_secs: 0,
        created_at: now,
        updated_at: now,
    }
}

fn sample_pool() -> PoolSession {
    let now = Utc::now();
    PoolSession {
        id: "pool-1".into(),
        name: "Demo Pool".into(),
        org_spec: "## Goal\nShip feature".into(),
        status: "active".into(),
        project_dir: Some("C:/demo".into()),
        task_timeout_secs: 0,
        last_active_at: Some(now),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn collab_trial_active_run_slot_overrides_idle_db_status() {
    assert_eq!(effective_trial_koi_status("idle", true), "busy");
}

#[test]
fn collab_trial_db_status_is_preserved_without_active_run_slot() {
    assert_eq!(effective_trial_koi_status("idle", false), "idle");
    assert_eq!(effective_trial_koi_status("busy", false), "busy");
}

#[test]
fn collab_trial_enrich_pool_message_metadata_records_structured_signal() {
    let metadata =
        enrich_pool_message_metadata(json!({}), "[ProjectStatus] ready_for_pisci_review @pisci");
    assert_eq!(
        metadata["coordination"]["signal"].as_str(),
        Some(STATUS_READY)
    );
    assert_eq!(
        metadata["coordination"]["mentions_pisci"].as_bool(),
        Some(true)
    );
    assert_eq!(metadata["mentions"]["pisci"].as_bool(), Some(true));
    assert_eq!(
        coordination_event_type_for_content("[ProjectStatus] ready_for_pisci_review"),
        Some("coordination_signal")
    );
}

#[test]
fn collab_trial_assessment_prefers_structured_coordination_metadata() {
    let messages = vec![sample_message(
        1,
        "koi-1",
        "plain text without legacy marker",
        json!({
            "coordination": {
                "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                "mentions_pisci": true,
            }
        }),
        Some("coordination_signal"),
    )];
    let assessment = assess_project_state(&messages, &[sample_todo("done")], &["koi-1".into()]);
    assert_eq!(assessment.decision, ProjectDecision::ReadyForPisciReview);
    assert_eq!(assessment.explicit_pisci_handoff_count, 1);
}

#[test]
fn collab_trial_assessment_keeps_legacy_text_signal_compatibility() {
    let messages = vec![sample_message(
        1,
        "koi-1",
        "[ProjectStatus] ready_for_pisci_review @pisci",
        json!({}),
        None,
    )];
    let assessment = assess_project_state(&messages, &[sample_todo("done")], &["koi-1".into()]);
    assert_eq!(assessment.ready_signal_count, 1);
    assert_eq!(assessment.explicit_pisci_handoff_count, 1);
}

#[test]
fn collab_trial_embedded_status_text_does_not_create_coordination_event() {
    assert_eq!(
        coordination_event_type_for_content(
            "Koi is working.\n\nTask payload:\nPlease inspect this context.\n[ProjectStatus] follow_up_needed @Reviewer"
        ),
        None
    );
}

#[test]
fn collab_trial_explicit_ready_handoff_overrides_stale_follow_up_once_work_is_done() {
    let messages = vec![
        sample_message(
            1,
            "architect",
            "[ProjectStatus] follow_up_needed @Coder",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::FollowUpNeeded.as_status_str(),
                    "mentions_pisci": false
                }
            }),
            Some("coordination_signal"),
        ),
        sample_message(
            2,
            "reviewer",
            "[ProjectStatus] ready_for_pisci_review @pisci",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                    "mentions_pisci": true
                }
            }),
            Some("coordination_signal"),
        ),
    ];
    let assessment = assess_project_state(
        &messages,
        &[sample_todo("done")],
        &["architect".into(), "reviewer".into()],
    );
    assert_eq!(assessment.decision, ProjectDecision::ReadyForPisciReview);
    assert_eq!(assessment.follow_up_signal_count, 1);
    assert_eq!(assessment.explicit_pisci_handoff_count, 1);
}

#[test]
fn collab_trial_all_done_without_handoff_requires_supervisor_decision() {
    let assessment = assess_project_state(&[], &[sample_todo("done")], &[]);
    assert_eq!(
        assessment.decision,
        ProjectDecision::SupervisorDecisionRequired
    );
    assert!(assessment.summary.contains("Pisci must inspect the pool"));
}

#[test]
fn collab_trial_needs_review_without_handoff_still_requests_pisci_review() {
    let assessment = assess_project_state(&[], &[sample_todo("needs_review")], &[]);
    assert_eq!(assessment.decision, ProjectDecision::ReadyForPisciReview);
    assert_eq!(assessment.needs_review_count, 1);
}

#[test]
fn collab_trial_coordination_event_digest_prioritizes_structured_events_and_target_mentions() {
    let messages = vec![
        sample_message(
            1,
            "koi-1",
            "@Reviewer please inspect this next",
            json!({}),
            Some("task_progress"),
        ),
        sample_message(
            2,
            "koi-2",
            "[ProjectStatus] follow_up_needed @Reviewer",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::FollowUpNeeded.as_status_str(),
                    "mentions_pisci": false
                }
            }),
            Some("coordination_signal"),
        ),
        sample_message(
            3,
            "koi-3",
            "Timeout while running tests",
            json!({}),
            Some("task_failed"),
        ),
    ];
    let digest = build_coordination_event_digest(
        &messages,
        EventDigestMode::CoordinationPlusFailures,
        &["Reviewer"],
        4,
        120,
    );
    assert_eq!(digest.lines.len(), 3);
    assert!(digest.lines.iter().any(|line| line.contains("coordination_signal")));
    assert!(digest.lines.iter().any(|line| line.contains("task_failed")));
    assert!(digest.lines.iter().any(|line| line.contains("target")));
}

#[test]
fn collab_trial_collect_pool_attention_uses_shared_pool_pisci_session_id() {
    let pool = sample_pool();
    let messages = vec![sample_message(
        1,
        "koi-1",
        "@pisci [ProjectStatus] waiting",
        json!({}),
        None,
    )];
    let todos = vec![sample_todo("in_progress")];
    let koi_ids = vec!["koi-1".to_string()];

    let attention = collect_pool_attention(&pool, &messages, &todos, &koi_ids, 0)
        .expect("attention should be raised for explicit pisci mention");
    assert_eq!(attention.session_id, "pisci_pool_pool-1");
    assert!(attention.summary.contains("Recent attention events: 1"));
}

#[test]
fn collab_trial_build_pool_heartbeat_message_keeps_no_archive_instruction() {
    let attention = PoolAttention {
        pool_id: "pool-1".into(),
        pool_name: "Demo Pool".into(),
        latest_message_id: 42,
        session_id: "pisci_pool_pool-1".into(),
        summary: "Pool summary".into(),
        assessment: ProjectAssessment {
            decision: ProjectDecision::ReadyForPisciReview,
            active_todo_count: 0,
            blocked_todo_count: 0,
            needs_review_count: 0,
            task_failed_count: 0,
            follow_up_signal_count: 0,
            ready_signal_count: 1,
            explicit_pisci_handoff_count: 1,
            attention_reasons: vec!["Project is ready for Pisci review".into()],
            summary: "Project looks ready".into(),
        },
    };

    let message = build_pool_heartbeat_message("Base prompt", &attention);
    assert!(message.contains("Do NOT archive the project during heartbeat"));
    assert!(message.contains("HEARTBEAT_OK"));
}

#[test]
fn collab_trial_heartbeat_scene_policy_is_lightweight_and_disables_proactive_compaction() {
    let policy = ScenePolicy::for_kind(SceneKind::HeartbeatSupervisor);
    assert_eq!(policy.registry_profile, RegistryProfile::HeartbeatSupervisor);
    assert!(!policy.allow_skill_loader);
    assert!(!policy.include_memory);
    assert!(!policy.include_task_state);
    assert!(!policy.include_project_instructions);
    assert_eq!(policy.effective_auto_compact_threshold(100_000), 0);
}

#[test]
fn collab_trial_main_chat_and_koi_scene_policies_keep_expected_context_sources() {
    let main_chat = ScenePolicy::for_kind(SceneKind::MainChat);
    assert!(main_chat.allow_skill_loader);
    assert!(main_chat.include_memory);
    assert!(main_chat.include_task_state);
    assert!(main_chat.include_pool_roster);

    let koi = ScenePolicy::for_kind(SceneKind::KoiTask);
    assert_eq!(koi.registry_profile, RegistryProfile::KoiTask);
    assert!(koi.allow_skill_loader);
    assert!(koi.include_memory);
    assert!(koi.include_pool_context);
    assert!(!koi.include_pool_roster);
    assert!(!koi.include_project_instructions);
    assert_eq!(koi.effective_auto_compact_threshold(100_000), 0);
}

#[test]
fn collab_trial_short_lived_headless_scenes_disable_proactive_compaction() {
    let pool = ScenePolicy::for_kind(SceneKind::PoolCoordinator);
    let im = ScenePolicy::for_kind(SceneKind::IMHeadless);
    assert_eq!(pool.effective_auto_compact_threshold(100_000), 0);
    assert_eq!(im.effective_auto_compact_threshold(100_000), 0);
}

#[test]
fn collab_trial_scene_budget_scales_from_total_input_budget() {
    let main_chat = ScenePolicy::for_kind(SceneKind::MainChat);
    let heartbeat = ScenePolicy::for_kind(SceneKind::HeartbeatSupervisor);
    let main_budget = main_chat.compute_injection_budget(128_000, 4_096);
    let heartbeat_budget = heartbeat.compute_injection_budget(128_000, 4_096);
    assert!(main_budget > heartbeat_budget);
    assert!(heartbeat_budget >= 1_200);
}

#[test]
fn collab_trial_collaboration_context_mode_matches_scene_boundaries() {
    assert_eq!(
        ScenePolicy::for_kind(SceneKind::MainChat).collaboration_context_mode(),
        CollaborationContextMode::OnDemand
    );
    assert_eq!(
        ScenePolicy::for_kind(SceneKind::IMHeadless).collaboration_context_mode(),
        CollaborationContextMode::Never
    );
    assert_eq!(
        ScenePolicy::for_kind(SceneKind::HeartbeatSupervisor).collaboration_context_mode(),
        CollaborationContextMode::Required
    );
}

#[test]
fn collab_trial_heartbeat_profile_keeps_only_coordination_tools() {
    let policy = ScenePolicy::for_kind(SceneKind::HeartbeatSupervisor);
    let allowlist = policy.tool_allowlist().expect("heartbeat allowlist");
    assert!(allowlist.contains(&"pool_org"));
    assert!(allowlist.contains(&"pool_chat"));
    assert!(!allowlist.contains(&"call_koi"));
    assert!(!allowlist.contains(&"plan_todo"));
}

#[test]
fn collab_trial_scene_slice_modes_match_collaboration_needs() {
    let main = ScenePolicy::for_kind(SceneKind::MainChat);
    assert_eq!(main.history_slice_mode(), HistorySliceMode::FullRecent);
    assert_eq!(main.event_digest_mode(), EventDigestMode::Off);
    assert_eq!(main.memory_slice_mode(), MemorySliceMode::ScopedSearch);
    assert_eq!(main.pool_snapshot_mode(), PoolSnapshotMode::Full);

    let pool = ScenePolicy::for_kind(SceneKind::PoolCoordinator);
    assert_eq!(pool.history_slice_mode(), HistorySliceMode::SummaryOnly);
    assert_eq!(
        pool.event_digest_mode(),
        EventDigestMode::CoordinationPlusFailures
    );
    assert_eq!(pool.memory_slice_mode(), MemorySliceMode::ScopedSearch);
    assert_eq!(pool.pool_snapshot_mode(), PoolSnapshotMode::Compact);

    let koi = ScenePolicy::for_kind(SceneKind::KoiTask);
    assert_eq!(koi.history_slice_mode(), HistorySliceMode::None);
    assert_eq!(
        koi.event_digest_mode(),
        EventDigestMode::CoordinationPlusFailures
    );
    assert_eq!(koi.memory_slice_mode(), MemorySliceMode::ScopedPlusRecent);
    assert_eq!(koi.pool_snapshot_mode(), PoolSnapshotMode::Compact);
}

// ─────────────────────────────────────────────────────────────────────────────
// Convergence suite
//
// These tests pin down the contract that "the pool working mechanism converges":
// every reachable state is either (a) clearly Continue with an attention reason
// that a heartbeat can act on, or (b) ReadyForPisciReview for Pisci to close.
// They also lock down temporal ordering so a single historical task_failed
// cannot indefinitely block an otherwise-resolved project.
// ─────────────────────────────────────────────────────────────────────────────

fn at_ms(base: DateTime<Utc>, offset_ms: i64) -> DateTime<Utc> {
    base + Duration::milliseconds(offset_ms)
}

fn message_at(
    id: i64,
    created_at: DateTime<Utc>,
    sender_id: &str,
    content: &str,
    metadata: serde_json::Value,
    event_type: Option<&str>,
) -> PoolMessage {
    PoolMessage {
        id,
        pool_session_id: "pool-1".into(),
        sender_id: sender_id.into(),
        content: content.into(),
        msg_type: "text".into(),
        metadata: metadata.to_string(),
        todo_id: None,
        reply_to_message_id: None,
        event_type: event_type.map(str::to_string),
        created_at,
    }
}

fn todo_at(status: &str, updated_at: DateTime<Utc>) -> KoiTodo {
    KoiTodo {
        id: "todo-1".into(),
        owner_id: "koi-1".into(),
        title: "Investigate".into(),
        description: "Check project state".into(),
        status: status.into(),
        priority: "medium".into(),
        assigned_by: "pisci".into(),
        pool_session_id: Some("pool-1".into()),
        claimed_by: Some("koi-1".into()),
        claimed_at: Some(updated_at),
        depends_on: None,
        blocked_reason: None,
        result_message_id: None,
        source_type: "pisci".into(),
        task_timeout_secs: 0,
        created_at: updated_at,
        updated_at,
    }
}

#[test]
fn convergence_task_failed_superseded_by_later_explicit_pisci_handoff_resolves() {
    // Failure happens first, then a later explicit @pisci handoff should let
    // the project converge. Before the temporal fix, a historical task_failed
    // would indefinitely block convergence — that is a non-convergent state.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let messages = vec![
        message_at(
            1,
            at_ms(base, 0),
            "koi-1",
            "Task timed out while running tests",
            json!({}),
            Some("task_failed"),
        ),
        message_at(
            2,
            at_ms(base, 1_000),
            "koi-1",
            "[ProjectStatus] ready_for_pisci_review @pisci",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                    "mentions_pisci": true,
                }
            }),
            Some("coordination_signal"),
        ),
    ];
    let assessment = assess_project_state(
        &messages,
        &[todo_at("done", at_ms(base, 1_200))],
        &["koi-1".into()],
    );
    assert_eq!(
        assessment.decision,
        ProjectDecision::ReadyForPisciReview,
        "explicit pisci handoff after a task_failed must re-open convergence"
    );
    assert_eq!(assessment.explicit_pisci_handoff_count, 1);
    assert_eq!(
        assessment.task_failed_count, 1,
        "the historical failure is still counted so Pisci can see it in the summary"
    );
}

#[test]
fn convergence_task_failed_after_latest_handoff_reopens_investigation() {
    // A failure posted AFTER the last explicit handoff must re-open the
    // project: agents cannot silently hand off once and then ignore later
    // breakage.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let messages = vec![
        message_at(
            1,
            at_ms(base, 0),
            "koi-1",
            "[ProjectStatus] ready_for_pisci_review @pisci",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                    "mentions_pisci": true,
                }
            }),
            Some("coordination_signal"),
        ),
        message_at(
            2,
            at_ms(base, 500),
            "koi-1",
            "Ran the build and it crashed again",
            json!({}),
            Some("task_failed"),
        ),
    ];
    let assessment = assess_project_state(
        &messages,
        &[todo_at("done", at_ms(base, 0))],
        &["koi-1".into()],
    );
    assert_eq!(assessment.decision, ProjectDecision::EscalateToHuman);
    assert!(
        assessment
            .attention_reasons
            .iter()
            .any(|r| r.contains("task_failed")),
        "a fresh failure after handoff must surface as an attention reason"
    );
}

#[test]
fn convergence_unresolved_task_failed_raises_human_escalation_attention() {
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let pool = sample_pool();
    let messages = vec![message_at(
        1,
        at_ms(base, 0),
        "koi-1",
        "Task timed out while running tests",
        json!({}),
        Some("task_failed"),
    )];
    let todos = vec![todo_at("done", at_ms(base, 500))];
    let attention = collect_pool_attention(&pool, &messages, &todos, &["koi-1".into()], 0)
        .expect("unresolved task_failed should raise heartbeat attention");
    assert_eq!(
        attention.assessment.decision,
        ProjectDecision::EscalateToHuman
    );
    assert!(
        attention
            .assessment
            .attention_reasons
            .iter()
            .any(|r| r.contains("human judgment")),
        "attention reason should explain the escalation boundary"
    );
}

#[test]
fn convergence_same_sender_flip_flop_prefers_latest_signal() {
    // If a reviewer first signals ready_for_pisci_review then retracts with
    // follow_up_needed, the latest signal must win so the project does not
    // prematurely converge on a stale "ready" state.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let messages = vec![
        message_at(
            1,
            at_ms(base, 0),
            "reviewer",
            "[ProjectStatus] ready_for_pisci_review @pisci",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                    "mentions_pisci": true,
                }
            }),
            Some("coordination_signal"),
        ),
        message_at(
            2,
            at_ms(base, 1_000),
            "reviewer",
            "[ProjectStatus] follow_up_needed @Coder",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::FollowUpNeeded.as_status_str(),
                    "mentions_pisci": false,
                }
            }),
            Some("coordination_signal"),
        ),
    ];
    let assessment = assess_project_state(
        &messages,
        &[todo_at("done", at_ms(base, 500))],
        &["reviewer".into()],
    );
    assert_eq!(assessment.decision, ProjectDecision::Continue);
    assert_eq!(assessment.follow_up_signal_count, 1);
    assert_eq!(assessment.explicit_pisci_handoff_count, 0);
    assert_eq!(assessment.ready_signal_count, 0);
}

#[test]
fn convergence_blocked_todo_without_signals_raises_attention_reason() {
    // A blocked todo with no coordination signals is still a convergence
    // risk: the pool has no active owner for that work item.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let assessment =
        assess_project_state(&[], &[todo_at("blocked", at_ms(base, 0))], &["koi-1".into()]);
    assert_eq!(assessment.decision, ProjectDecision::Continue);
    assert_eq!(assessment.blocked_todo_count, 1);
    assert!(
        assessment
            .attention_reasons
            .iter()
            .any(|r| r.contains("blocked")),
        "blocked todos must surface attention reasons so heartbeat acts on them"
    );
}

#[test]
fn convergence_needs_review_outlives_older_task_failed() {
    // `needs_review` is a live todo state, not a historical event. When a
    // newer needs_review state exists, an older task_failed must not block
    // Pisci-review convergence.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let messages = vec![message_at(
        1,
        at_ms(base, 0),
        "koi-1",
        "Earlier run crashed",
        json!({}),
        Some("task_failed"),
    )];
    let todos = vec![todo_at("needs_review", at_ms(base, 1_500))];
    let assessment = assess_project_state(&messages, &todos, &["koi-1".into()]);
    assert_eq!(assessment.decision, ProjectDecision::ReadyForPisciReview);
    assert_eq!(assessment.needs_review_count, 1);
    assert_eq!(assessment.task_failed_count, 1);
}

#[test]
fn convergence_collect_pool_attention_flags_ready_without_explicit_pisci_handoff() {
    // "[ProjectStatus] ready_for_pisci_review" without `@pisci` is a stuck
    // state: no one explicitly handed the project to Pisci. Heartbeat must
    // still raise attention so Pisci inspects the pool.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let pool = sample_pool();
    let messages = vec![message_at(
        10,
        at_ms(base, 0),
        "koi-1",
        "[ProjectStatus] ready_for_pisci_review",
        json!({
            "coordination": {
                "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                "mentions_pisci": false,
            }
        }),
        Some("coordination_signal"),
    )];
    let todos = vec![todo_at("done", at_ms(base, 0))];
    let koi_ids = vec!["koi-1".to_string()];
    let attention = collect_pool_attention(&pool, &messages, &todos, &koi_ids, 0)
        .expect("ready-without-handoff must raise attention");
    assert_eq!(
        attention.assessment.decision,
        ProjectDecision::SupervisorDecisionRequired
    );
    assert!(
        attention
            .assessment
            .attention_reasons
            .iter()
            .any(|r| r.contains("did not explicitly hand off")),
        "attention reason should name the missing @pisci handoff"
    );
}

#[test]
fn convergence_all_done_without_handoff_raises_supervisor_decision_attention() {
    let pool = sample_pool();
    let todos = vec![sample_todo("done")];
    let attention = collect_pool_attention(&pool, &[], &todos, &[], 0)
        .expect("all-done-without-handoff should still require Pisci attention");
    assert_eq!(
        attention.assessment.decision,
        ProjectDecision::SupervisorDecisionRequired
    );
    assert!(
        attention
            .assessment
            .attention_reasons
            .iter()
            .any(|r| r.contains("Pisci must make the next global decision")),
        "attention reason should explain why supervisor judgment is still required"
    );
}

#[test]
fn convergence_silent_dormant_state_does_not_raise_attention() {
    // Pool with no todos, no signals, and no new messages is a benign
    // dormant state: heartbeat must not waste attention on it.
    let pool = sample_pool();
    let attention = collect_pool_attention(&pool, &[], &[], &[], 0);
    assert!(
        attention.is_none(),
        "fully quiescent pools should not raise attention"
    );
}

#[test]
fn convergence_collect_pool_attention_ignores_already_seen_events() {
    // Heartbeat cursor must advance: once an attention event has been seen,
    // it should not re-fire. Otherwise the pool never drains and heartbeat
    // oscillates instead of converging.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let pool = sample_pool();
    let messages = vec![message_at(
        7,
        at_ms(base, 0),
        "koi-1",
        "@pisci please take a look",
        json!({}),
        None,
    )];
    let todos = vec![todo_at("done", at_ms(base, 0))];
    let koi_ids = vec!["koi-1".to_string()];

    let first = collect_pool_attention(&pool, &messages, &todos, &koi_ids, 0)
        .expect("first scan must raise attention");
    assert_eq!(first.latest_message_id, 7);

    let second = collect_pool_attention(&pool, &messages, &todos, &koi_ids, 7);
    assert!(
        second.is_none(),
        "after advancing the cursor past the @pisci mention, heartbeat must stay quiet"
    );
}

#[test]
fn convergence_multiple_handoffs_across_senders_aggregate_cleanly() {
    // When several senders converge in parallel on ready_for_pisci_review
    // with @pisci handoff and no unfinished work remains, the assessment
    // must report Ready with all handoffs counted. This guards against
    // silent reducer bugs that drop signals from non-first senders.
    let base = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
    let mut messages = Vec::new();
    for (idx, sender) in ["architect", "coder", "reviewer"].iter().enumerate() {
        messages.push(message_at(
            (idx + 1) as i64,
            at_ms(base, idx as i64 * 100),
            sender,
            "[ProjectStatus] ready_for_pisci_review @pisci",
            json!({
                "coordination": {
                    "signal": CoordinationSignalKind::ReadyForPisciReview.as_status_str(),
                    "mentions_pisci": true,
                }
            }),
            Some("coordination_signal"),
        ));
    }
    let todos = vec![todo_at("done", at_ms(base, 500))];
    let koi_ids = vec![
        "architect".to_string(),
        "coder".to_string(),
        "reviewer".to_string(),
    ];
    let assessment = assess_project_state(&messages, &todos, &koi_ids);
    assert_eq!(assessment.decision, ProjectDecision::ReadyForPisciReview);
    assert_eq!(assessment.explicit_pisci_handoff_count, 3);
    assert_eq!(assessment.ready_signal_count, 3);
    assert_eq!(assessment.follow_up_signal_count, 0);
}
