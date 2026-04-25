//! Unit tests for the `DesktopEventSink` → Tauri event-name bridge.
//!
//! The real sink lives inside [`pisci_desktop_lib::host`] and calls
//! `AppHandle::emit`, which is impossible to drive in a plain
//! integration test (constructing a test `AppHandle` would spin up the
//! WebView2 loader on Windows and fail with the known
//! `STATUS_ENTRYPOINT_NOT_FOUND` error).
//!
//! Instead we assert the wire-format contract through the pure
//! [`pool_event_envelopes`] mapper extracted in Phase 1.8. The invariant
//! under test is:
//!
//! 1. Every [`PoolEvent`] variant maps to at least one Tauri emit pair.
//! 2. The per-variant event names match the names the React frontend
//!    has been subscribing to since Phase 1.4 (regression guard).
//! 3. The canonical forward-compatible channel constant is exported as
//!    a public string the frontend can rely on.

use pisci_core::host::{
    PoolEvent, PoolMessageSnapshot, PoolSessionSnapshot, PoolWaitSummary, TodoChangeAction,
    TodoSnapshot,
};
use pisci_desktop_lib::host::{pool_event_envelopes, POOL_EVENT_CANONICAL_CHANNEL};

fn sample_pool_snapshot() -> PoolSessionSnapshot {
    PoolSessionSnapshot {
        id: "pool-xyz".into(),
        name: "unit-test pool".into(),
        status: "active".into(),
        project_dir: None,
        task_timeout_secs: 0,
    }
}

fn sample_message_snapshot() -> PoolMessageSnapshot {
    PoolMessageSnapshot {
        id: 42,
        pool_session_id: "pool-xyz".into(),
        sender_id: "pisci".into(),
        content: "hi".into(),
        msg_type: "text".into(),
        metadata: serde_json::Value::Null,
        todo_id: None,
        reply_to_message_id: None,
        event_type: None,
        created_at: chrono::Utc::now(),
    }
}

fn sample_wait_summary() -> PoolWaitSummary {
    PoolWaitSummary {
        completed: true,
        timed_out: false,
        closeout_status: "awaiting_supervisor_closeout".into(),
        requires_supervisor_closeout: true,
        active_todos: 0,
        done_todos: 3,
        cancelled_todos: 0,
        blocked_todos: 0,
        latest_messages: vec!["done".into()],
    }
}

fn sample_todo_snapshot() -> TodoSnapshot {
    TodoSnapshot {
        id: "todo-1".into(),
        owner_id: "koi-alpha".into(),
        title: "do a thing".into(),
        description: String::new(),
        status: "todo".into(),
        priority: "medium".into(),
        assigned_by: "pisci".into(),
        pool_session_id: Some("pool-xyz".into()),
        claimed_by: None,
        depends_on: None,
        blocked_reason: None,
        result_message_id: None,
        source_type: "koi".into(),
        task_timeout_secs: 0,
    }
}

#[test]
fn canonical_channel_is_stable() {
    // The frontend hardcodes `host://pool_event`; changing this string
    // breaks every downstream subscriber.
    assert_eq!(POOL_EVENT_CANONICAL_CHANNEL, "host://pool_event");
}

#[test]
fn every_pool_event_variant_produces_one_pair() {
    let pool = sample_pool_snapshot();
    let msg = sample_message_snapshot();
    let todo = sample_todo_snapshot();

    let cases: Vec<(PoolEvent, &str)> = vec![
        (
            PoolEvent::PoolCreated { pool: pool.clone() },
            "pool_session_created",
        ),
        (
            PoolEvent::PoolUpdated { pool: pool.clone() },
            "pool_session_updated",
        ),
        (
            PoolEvent::PoolPaused { pool: pool.clone() },
            "pool_session_updated",
        ),
        (
            PoolEvent::PoolResumed { pool: pool.clone() },
            "pool_session_updated",
        ),
        (
            PoolEvent::PoolArchived {
                pool_id: "pool-xyz".into(),
            },
            "pool_session_updated",
        ),
        (
            PoolEvent::MessageAppended {
                pool_id: "pool-xyz".into(),
                message: msg.clone(),
            },
            "pool_message_pool-xyz",
        ),
        (
            PoolEvent::TodoChanged {
                pool_id: "pool-xyz".into(),
                action: TodoChangeAction::Created,
                todo: todo.clone(),
            },
            "koi_todo_updated",
        ),
        (
            PoolEvent::KoiAssigned {
                pool_id: "pool-xyz".into(),
                koi_id: "koi-alpha".into(),
                todo_id: "todo-1".into(),
            },
            "koi_status_changed",
        ),
        (
            PoolEvent::KoiStatusChanged {
                pool_id: "pool-xyz".into(),
                koi_id: "koi-alpha".into(),
                status: "running".into(),
            },
            "koi_status_changed",
        ),
        (
            PoolEvent::KoiStaleRecovered {
                pool_id: "pool-xyz".into(),
                koi_id: "koi-alpha".into(),
                recovered_todo_count: 2,
            },
            "koi_status_changed",
        ),
        (
            PoolEvent::CoordinatorIdle {
                pool_id: "pool-xyz".into(),
            },
            "pool_coordinator_idle",
        ),
        (
            PoolEvent::CoordinatorCompleted {
                pool_id: "pool-xyz".into(),
                summary: sample_wait_summary(),
            },
            "pool_coordinator_completed",
        ),
        (
            PoolEvent::CoordinatorTimedOut {
                pool_id: "pool-xyz".into(),
                summary: sample_wait_summary(),
            },
            "pool_coordinator_timed_out",
        ),
        (
            PoolEvent::FishProgress {
                parent_session_id: "sess-1".into(),
                fish_id: "fish-1".into(),
                stage: "running".into(),
                payload: Some(serde_json::Value::Null),
            },
            "fish_progress_sess-1",
        ),
    ];

    for (event, expected_name) in cases {
        let pairs = pool_event_envelopes(&event);
        assert_eq!(
            pairs.len(),
            1,
            "variant {:?} should emit exactly one frame, got {} frames",
            event.kind(),
            pairs.len()
        );
        assert_eq!(
            pairs[0].0,
            expected_name,
            "variant {:?} should emit to `{}`, got `{}`",
            event.kind(),
            expected_name,
            pairs[0].0
        );
    }
}

#[test]
fn message_channel_carries_pool_id_suffix() {
    let event = PoolEvent::MessageAppended {
        pool_id: "abc-123".into(),
        message: sample_message_snapshot(),
    };
    let pairs = pool_event_envelopes(&event);
    assert_eq!(pairs[0].0, "pool_message_abc-123");
}

#[test]
fn archived_payload_carries_status_flag() {
    let event = PoolEvent::PoolArchived {
        pool_id: "pool-xyz".into(),
    };
    let pairs = pool_event_envelopes(&event);
    let payload = &pairs[0].1;
    assert_eq!(payload["id"], "pool-xyz");
    assert_eq!(payload["status"], "archived");
}

#[test]
fn todo_changed_payload_snapshots_action_and_todo() {
    let todo = sample_todo_snapshot();
    let event = PoolEvent::TodoChanged {
        pool_id: "pool-xyz".into(),
        action: TodoChangeAction::Claimed,
        todo: todo.clone(),
    };
    let pairs = pool_event_envelopes(&event);
    let payload = &pairs[0].1;
    assert_eq!(payload["id"], todo.id);
    assert_eq!(payload["pool_id"], "pool-xyz");
    assert_eq!(payload["action"], "claimed");
    assert_eq!(payload["todo"]["title"], todo.title);
}
