/// Test Runner — run multi-agent integration tests inside the Tauri runtime.
///
/// Uses in-memory SQLite + LogEventBus to validate the full
/// collaboration pipeline without calling real LLMs.
use crate::agent::host::HostAgent;
use crate::commands::collab_trial::assess_trial_project_state;
use crate::koi::event_bus::LogEventBus;
use crate::koi::runtime::KoiRuntime;
use crate::koi::{KoiDefinition, KoiTodo, PoolMessage};
use crate::pisci::heartbeat::{build_pool_heartbeat_message, collect_pool_attention};
use crate::pisci::project_state::ProjectDecision as TrialDecision;
use crate::store::Database;
use chrono::Utc;
use rusqlite;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub message: String,
    pub message_key: Option<String>,
    pub message_params: Option<Value>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSuiteResult {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<TestResult>,
    pub summary: String,
}

fn setup() -> (Arc<Mutex<Database>>, LogEventBus, KoiRuntime) {
    let db = Database::open_in_memory().expect("in-memory DB");
    let db = Arc::new(Mutex::new(db));
    let bus = LogEventBus::new(db.clone());
    let runtime = KoiRuntime::new(Arc::new(bus.clone()));
    (db, bus, runtime)
}

#[derive(Debug, Clone)]
struct LocalizedError {
    key: &'static str,
    params: Value,
    fallback: String,
}

fn fail(key: &'static str, fallback: impl Into<String>) -> LocalizedError {
    LocalizedError {
        key,
        params: json!({}),
        fallback: fallback.into(),
    }
}

fn fail_with_params(
    key: &'static str,
    params: Value,
    fallback: impl Into<String>,
) -> LocalizedError {
    LocalizedError {
        key,
        params,
        fallback: fallback.into(),
    }
}

fn backend_err(err: impl ToString) -> LocalizedError {
    let detail = err.to_string();
    fail_with_params(
        "debug.multiAgentErrBackend",
        json!({ "detail": detail }),
        detail,
    )
}

fn finish_test(
    name: &str,
    result: Result<(), LocalizedError>,
    start: std::time::Instant,
) -> TestResult {
    match result {
        Ok(()) => TestResult {
            name: name.into(),
            passed: true,
            message: String::new(),
            message_key: None,
            message_params: None,
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(err) => TestResult {
            name: name.into(),
            passed: false,
            message: err.fallback,
            message_key: Some(err.key.into()),
            message_params: Some(err.params),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

#[tauri::command]
pub async fn run_multi_agent_tests() -> Result<TestSuiteResult, String> {
    let mut results: Vec<TestResult> = Vec::new();

    results.push(test_koi_crud().await);
    results.push(test_memory_scoping().await);
    results.push(test_memory_project_priority().await);
    results.push(test_project_completion_assessment().await);
    results.push(test_pisci_heartbeat_attention_scan().await);
    results.push(test_pisci_heartbeat_prompt_guardrails().await);
    results.push(test_todo_lifecycle().await);
    results.push(test_pool_messages().await);
    results.push(test_route_to_koi().await);
    results.push(test_runtime_assign_execute().await);
    results.push(test_runtime_mention().await);
    results.push(test_at_all_mention().await);
    results.push(test_pool_chat_conversation().await);
    results.push(test_full_e2e().await);
    results.push(test_koi_limit().await);
    results.push(test_vacation_cancels_todos().await);
    results.push(test_watchdog_recover().await);
    results.push(test_watchdog_recover_zero_threshold().await);
    results.push(test_activate_pending_todos_no_duplicates().await);
    results.push(test_starter_koi_seed().await);
    results.push(test_pool_project_dir().await);
    results.push(test_pool_session_prefix_lookup().await);
    results.push(test_headless_session_scope_isolation().await);
    results.push(test_runtime_identifier_canonicalization().await);
    results.push(test_archived_pool_blocks_runtime_work().await);
    results.push(test_recover_stale_running_sessions().await);

    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let total = results.len();
    let summary = if failed == 0 {
        format!("ALL {} TESTS PASSED", total)
    } else {
        format!("{}/{} FAILED", failed, total)
    };

    Ok(TestSuiteResult {
        total,
        passed,
        failed,
        results,
        summary,
    })
}

fn ok() -> Result<(), LocalizedError> {
    Ok(())
}

async fn test_koi_crud() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let koi = db
            .create_koi(
                "Architect",
                "架构师",
                "🏗️",
                "#7c6af7",
                "Design systems.",
                "System architect",
            )
            .map_err(backend_err)?;
        if koi.name != "Architect" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"koi.name","expected":"Architect","actual":koi.name}),
                "name mismatch",
            ));
        }
        if koi.status != "idle" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"koi.status","expected":"idle","actual":koi.status}),
                "should start idle",
            ));
        }
        let fetched = db.get_koi(&koi.id).map_err(backend_err)?.ok_or_else(|| {
            fail_with_params(
                "debug.multiAgentErrMissing",
                json!({"subject":"koi"}),
                "not found",
            )
        })?;
        if fetched.name != "Architect" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"fetched.name","expected":"Architect","actual":fetched.name}),
                "fetch mismatch",
            ));
        }
        db.update_koi_status(&koi.id, "busy").map_err(backend_err)?;
        let u = db.get_koi(&koi.id).map_err(backend_err)?.unwrap();
        if u.status != "busy" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"updated.status","expected":"busy","actual":u.status}),
                "status not updated",
            ));
        }
        db.delete_koi(&koi.id).map_err(backend_err)?;
        if db.get_koi(&koi.id).map_err(backend_err)?.is_some() {
            return Err(fail_with_params(
                "debug.multiAgentErrUnexpectedSome",
                json!({"subject":"deleted koi"}),
                "not deleted",
            ));
        }
        ok()
    }
    .await;
    finish_test("koi_crud", r, start)
}

async fn test_memory_scoping() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        db.save_memory(
            "Pisci goal",
            "project",
            0.9,
            Some("s1"),
            "pisci",
            "private",
            "pisci",
            None,
        )
        .map_err(backend_err)?;
        db.save_memory(
            "KoiA schema",
            "fact",
            0.85,
            Some("s2"),
            "koi-a",
            "private",
            "koi-a",
            None,
        )
        .map_err(backend_err)?;
        db.save_memory(
            "Shared conv",
            "project",
            0.9,
            Some("s3"),
            "pisci",
            "project",
            "pool-1",
            None,
        )
        .map_err(backend_err)?;
        let p = db.list_memories_for_owner("pisci").map_err(backend_err)?;
        if p.len() != 2 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"pisci memories","expected":2,"actual":p.len()}),
                format!("pisci: expected 2, got {}", p.len()),
            ));
        }
        let k = db.list_memories_for_owner("koi-a").map_err(backend_err)?;
        if k.len() != 1 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"koi-a memories","expected":1,"actual":k.len()}),
                format!("koi-a: expected 1, got {}", k.len()),
            ));
        }
        ok()
    }
    .await;
    finish_test("memory_scoping", r, start)
}

async fn test_memory_project_priority() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        // Cross-project skill (project_scope_id = None) — visible in all projects
        db.save_memory("Rust async skill", "fact", 0.9, Some("s1"), "coder", "private", "coder", None).map_err(backend_err)?;
        // Project-specific memory for pool-A (project_scope_id = "pool-a")
        db.save_memory("Pool-A architecture decision", "fact", 0.9, Some("s2"), "coder", "private", "coder", Some("pool-a")).map_err(backend_err)?;
        // Project-specific memory for pool-B
        db.save_memory("Pool-B API design", "fact", 0.9, Some("s3"), "coder", "private", "coder", Some("pool-b")).map_err(backend_err)?;

        // list_memories_for_owner should return all 3 (no project filter)
        let all = db.list_memories_for_owner("coder").map_err(backend_err)?;
        if all.len() != 3 {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"all coder memories","expected":3,"actual":all.len()}), format!("expected 3, got {}", all.len())));
        }

        // Query the project-specific keyword: in pool-a it should find only pool-a memory, not pool-b.
        let pool_a_arch = db.search_memories_scoped("architecture", "coder", Some("pool-a"), 10).map_err(backend_err)?;
        if !pool_a_arch.iter().any(|m| m.project_scope_id.as_deref() == Some("pool-a")) {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"pool-A project memory","expected":"present","actual":"missing"}), "pool-A specific memory missing from scoped search"));
        }
        if pool_a_arch.iter().any(|m| m.project_scope_id.as_deref() == Some("pool-b")) {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"pool-B memory isolation","expected":"not leaked","actual":"leaked"}), "pool-B memory leaked into pool-A search"));
        }

        // Query the cross-project keyword: it should be visible both with and without pool context.
        let pool_a_skill = db.search_memories_scoped("skill", "coder", Some("pool-a"), 10).map_err(backend_err)?;
        if !pool_a_skill.iter().any(|m| m.project_scope_id.is_none()) {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"cross-project memory in pool-A","expected":"present","actual":"missing"}), "cross-project skill memory missing from pool-A scoped search"));
        }

        // Without pool context, project-specific memories must not leak, but cross-project ones remain visible.
        let no_pool_arch = db.search_memories_scoped("architecture", "coder", None, 10).map_err(backend_err)?;
        if no_pool_arch.iter().any(|m| m.project_scope_id.is_some()) {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"no-pool architecture isolation","expected":"no project-specific memories","actual":"project-specific leaked"}), "project-specific memory leaked into no-pool architecture search"));
        }
        let no_pool_skill = db.search_memories_scoped("skill", "coder", None, 10).map_err(backend_err)?;
        if !no_pool_skill.iter().any(|m| m.project_scope_id.is_none()) {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"no-pool cross-project memory","expected":"present","actual":"missing"}), "cross-project memory missing from no-pool search"));
        }

        ok()
    }.await;
    finish_test("memory_project_priority", r, start)
}

async fn test_project_completion_assessment() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let now = Utc::now();
        let koi_ids = vec!["architect".to_string(), "coder".to_string(), "reviewer".to_string()];

        // Scenario 1: active todo means the project must continue.
        let active_todos = vec![KoiTodo {
            id: "todo-1".into(),
            owner_id: "coder".into(),
            title: "Implement".into(),
            description: "".into(),
            status: "in_progress".into(),
            priority: "high".into(),
            assigned_by: "pisci".into(),
            pool_session_id: Some("pool-1".into()),
            claimed_by: Some("coder".into()),
            claimed_at: Some(now),
            depends_on: None,
            blocked_reason: None,
            result_message_id: None,
            source_type: "pisci".into(),
            created_at: now,
            updated_at: now,
        }];
        let continue_assessment = assess_trial_project_state(&[], &active_todos, &koi_ids);
        if continue_assessment.decision != TrialDecision::Continue {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"assessment with active todo","expected":"Continue","actual":format!("{:?}", continue_assessment.decision)}),
                "active todo should keep project running",
            ));
        }

        // Scenario 2: reviewer requests follow-up, no active todos yet — still should continue.
        let follow_up_messages = vec![PoolMessage {
            id: 1,
            pool_session_id: "pool-1".into(),
            sender_id: "reviewer".into(),
            content: "[ProjectStatus] follow_up_needed @Coder Please address edge cases.".into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        let follow_up_assessment = assess_trial_project_state(&follow_up_messages, &[], &koi_ids);
        if follow_up_assessment.decision != TrialDecision::Continue {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"assessment with follow-up signal","expected":"Continue","actual":format!("{:?}", follow_up_assessment.decision)}),
                "follow-up signal should keep project running",
            ));
        }

        // Scenario 3: plain messages without signals should NOT imply completion.
        let plain_messages = vec![PoolMessage {
            id: 2,
            pool_session_id: "pool-1".into(),
            sender_id: "reviewer".into(),
            content: "I think we are close, but this is still just a normal chat message.".into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        let plain_assessment = assess_trial_project_state(&plain_messages, &[], &koi_ids);
        if plain_assessment.decision != TrialDecision::Continue {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"assessment with plain messages","expected":"Continue","actual":format!("{:?}", plain_assessment.decision)}),
                "plain messages should not imply project completion",
            ));
        }

        // Scenario 4: ready signal without @pisci should still continue.
        let ready_without_handoff = vec![PoolMessage {
            id: 3,
            pool_session_id: "pool-1".into(),
            sender_id: "reviewer".into(),
            content: "[ProjectStatus] ready_for_pisci_review The work looks consistent now.".into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        let ready_without_handoff_assessment = assess_trial_project_state(&ready_without_handoff, &[], &koi_ids);
        if ready_without_handoff_assessment.decision != TrialDecision::Continue {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"assessment with ready signal but no @pisci","expected":"Continue","actual":format!("{:?}", ready_without_handoff_assessment.decision)}),
                "ready signal without @pisci should not finish the project",
            ));
        }

        // Scenario 5: no active todos and ready signal with @pisci -> Pisci can review for wrap-up.
        let ready_messages = vec![PoolMessage {
            id: 4,
            pool_session_id: "pool-1".into(),
            sender_id: "reviewer".into(),
            content: "[ProjectStatus] ready_for_pisci_review @pisci The work looks consistent now.".into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        let ready_assessment = assess_trial_project_state(&ready_messages, &[], &koi_ids);
        if ready_assessment.decision != TrialDecision::ReadyForPisciReview {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"assessment with ready signal","expected":"ReadyForPisciReview","actual":format!("{:?}", ready_assessment.decision)}),
                "ready signal should hand control back to Pisci",
            ));
        }

        // Scenario 6: latest signal per sender wins, so follow-up after ready reopens the project.
        let mixed_messages = vec![
            PoolMessage {
                id: 5,
                pool_session_id: "pool-1".into(),
                sender_id: "reviewer".into(),
                content: "[ProjectStatus] ready_for_pisci_review @pisci Looks ready.".into(),
                msg_type: "text".into(),
                metadata: "{}".into(),
                todo_id: None,
                reply_to_message_id: None,
                event_type: None,
                created_at: now,
            },
            PoolMessage {
                id: 6,
                pool_session_id: "pool-1".into(),
                sender_id: "reviewer".into(),
                content: "[ProjectStatus] follow_up_needed @Coder One edge case still fails.".into(),
                msg_type: "text".into(),
                metadata: "{}".into(),
                todo_id: None,
                reply_to_message_id: None,
                event_type: None,
                created_at: now,
            },
        ];
        let mixed_assessment = assess_trial_project_state(&mixed_messages, &[], &koi_ids);
        if mixed_assessment.decision != TrialDecision::Continue {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"assessment with latest follow-up","expected":"Continue","actual":format!("{:?}", mixed_assessment.decision)}),
                "latest follow-up signal should reopen the project",
            ));
        }

        ok()
    }.await;
    finish_test("project_completion_assessment", r, start)
}

async fn test_pisci_heartbeat_attention_scan() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let now = Utc::now();
        let (pool, reviewer) = {
            let db = db.lock().await;
            let reviewer = db.create_koi("Reviewer", "代码审查员", "🔍", "#26de81", "Review.", "Reviewer")
                .map_err(backend_err)?;
            let pool = db.create_pool_session("HeartbeatPool").map_err(backend_err)?;
            (pool, reviewer)
        };
        let koi_ids = vec![reviewer.id.clone()];

        let ordinary = vec![PoolMessage {
            id: 1,
            pool_session_id: pool.id.clone(),
            sender_id: reviewer.id.clone(),
            content: "普通讨论消息，不应触发 Pisci attention。".into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        if collect_pool_attention(&pool, &ordinary, &[], &koi_ids, 0).is_some() {
            return Err(fail_with_params(
                "debug.multiAgentErrUnexpectedSome",
                json!({"subject":"ordinary pool message attention"}),
                "ordinary messages should not trigger Pisci attention",
            ));
        }

        let ready_without_handoff = vec![
            ordinary[0].clone(),
            PoolMessage {
                id: 2,
                pool_session_id: pool.id.clone(),
                sender_id: reviewer.id.clone(),
                content: "[ProjectStatus] ready_for_pisci_review Work appears stable.".into(),
                msg_type: "text".into(),
                metadata: "{}".into(),
                todo_id: None,
                reply_to_message_id: None,
                event_type: None,
                created_at: now,
            },
        ];
        let attention = collect_pool_attention(&pool, &ready_without_handoff, &[], &koi_ids, 1)
            .ok_or_else(|| fail("debug.multiAgentErrMissing", "ready signal should trigger heartbeat attention"))?;
        if attention.assessment.decision != TrialDecision::Continue {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"attention without @pisci","expected":"Continue","actual":format!("{:?}", attention.assessment.decision)}),
                "heartbeat attention without @pisci should not mark project ready",
            ));
        }

        let ready_with_handoff = vec![
            ordinary[0].clone(),
            ready_without_handoff[1].clone(),
            PoolMessage {
                id: 3,
                pool_session_id: pool.id.clone(),
                sender_id: reviewer.id.clone(),
                content: "[ProjectStatus] ready_for_pisci_review @pisci Please review the pool.".into(),
                msg_type: "text".into(),
                metadata: "{}".into(),
                todo_id: None,
                reply_to_message_id: None,
                event_type: None,
                created_at: now,
            },
        ];
        let attention = collect_pool_attention(&pool, &ready_with_handoff, &[], &koi_ids, 2)
            .ok_or_else(|| fail("debug.multiAgentErrMissing", "explicit @pisci handoff should trigger attention"))?;
        if attention.assessment.decision != TrialDecision::ReadyForPisciReview {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"attention with @pisci","expected":"ReadyForPisciReview","actual":format!("{:?}", attention.assessment.decision)}),
                "explicit @pisci handoff should make the project review-ready",
            ));
        }

        ok()
    }.await;
    finish_test("pisci_heartbeat_attention_scan", r, start)
}

async fn test_pisci_heartbeat_prompt_guardrails() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let now = Utc::now();
        let (pool, reviewer) = {
            let db = db.lock().await;
            let reviewer = db
                .create_koi(
                    "Reviewer",
                    "代码审查员",
                    "🔍",
                    "#26de81",
                    "Review.",
                    "Reviewer",
                )
                .map_err(backend_err)?;
            let pool = db
                .create_pool_session("PromptGuardPool")
                .map_err(backend_err)?;
            (pool, reviewer)
        };
        let koi_ids = vec![reviewer.id.clone()];
        let stalled_follow_up = vec![PoolMessage {
            id: 1,
            pool_session_id: pool.id.clone(),
            sender_id: reviewer.id.clone(),
            content: "[ProjectStatus] follow_up_needed @Coder Please continue the remaining fixes."
                .into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        let attention = collect_pool_attention(&pool, &stalled_follow_up, &[], &koi_ids, 0)
            .ok_or_else(|| {
                fail(
                    "debug.multiAgentErrMissing",
                    "stalled follow-up should trigger attention",
                )
            })?;
        let prompt = build_pool_heartbeat_message("Base heartbeat prompt", &attention);
        if !prompt.contains("NOT ready for HEARTBEAT_OK") {
            return Err(fail_with_params(
                "debug.multiAgentErrMissing",
                json!({"subject":"heartbeat guardrail against HEARTBEAT_OK"}),
                "heartbeat prompt should forbid HEARTBEAT_OK when project must continue",
            ));
        }
        if !prompt.contains("re-open the project") {
            return Err(fail_with_params(
                "debug.multiAgentErrMissing",
                json!({"subject":"heartbeat re-open coordination guidance"}),
                "heartbeat prompt should tell Pisci to re-open stalled follow-up work",
            ));
        }

        let ready_messages = vec![PoolMessage {
            id: 2,
            pool_session_id: pool.id.clone(),
            sender_id: reviewer.id.clone(),
            content: "[ProjectStatus] ready_for_pisci_review @pisci Please review the pool.".into(),
            msg_type: "text".into(),
            metadata: "{}".into(),
            todo_id: None,
            reply_to_message_id: None,
            event_type: None,
            created_at: now,
        }];
        let ready_attention = collect_pool_attention(&pool, &ready_messages, &[], &koi_ids, 0)
            .ok_or_else(|| {
                fail(
                    "debug.multiAgentErrMissing",
                    "ready-for-review handoff should trigger attention",
                )
            })?;
        let ready_prompt = build_pool_heartbeat_message("Base heartbeat prompt", &ready_attention);
        if !ready_prompt.contains("HEARTBEAT_OK is still not automatic") {
            return Err(fail_with_params(
                "debug.multiAgentErrMissing",
                json!({"subject":"heartbeat review-ready caution"}),
                "review-ready heartbeat prompt should still warn against automatic completion",
            ));
        }

        ok()
    }
    .await;
    finish_test("pisci_heartbeat_prompt_guardrails", r, start)
}

async fn test_todo_lifecycle() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let koi = db
            .create_koi("Worker", "通用助理", "⚡", "#45b7d1", "Work", "Worker")
            .map_err(backend_err)?;
        let pool = db.create_pool_session("WorkPool").map_err(backend_err)?;
        let todo = db
            .create_koi_todo(
                &koi.id,
                "Build auth",
                "JWT",
                "high",
                "pisci",
                Some(&pool.id),
                "pisci",
                None,
            )
            .map_err(backend_err)?;
        if todo.status != "todo" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"todo.status","expected":"todo","actual":todo.status}),
                "should be todo",
            ));
        }
        db.claim_koi_todo(&todo.id, &koi.id).map_err(backend_err)?;
        let c = db.get_koi_todo(&todo.id).map_err(backend_err)?.unwrap();
        if c.status != "in_progress" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"claimed todo.status","expected":"in_progress","actual":c.status}),
                "should be in_progress",
            ));
        }
        db.complete_koi_todo(&todo.id, Some(42))
            .map_err(backend_err)?;
        let d = db.get_koi_todo(&todo.id).map_err(backend_err)?.unwrap();
        if d.status != "done" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"completed todo.status","expected":"done","actual":d.status}),
                "should be done",
            ));
        }
        let t2 = db
            .create_koi_todo(
                &koi.id,
                "Deploy",
                "",
                "urgent",
                "pisci",
                Some(&pool.id),
                "pisci",
                Some(&todo.id),
            )
            .map_err(backend_err)?;
        db.claim_koi_todo(&t2.id, &koi.id).map_err(backend_err)?;
        db.block_koi_todo(&t2.id, "Blocked").map_err(backend_err)?;
        let b = db.get_koi_todo(&t2.id).map_err(backend_err)?.unwrap();
        if b.status != "blocked" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"blocked todo.status","expected":"blocked","actual":b.status}),
                "should be blocked",
            ));
        }
        ok()
    }
    .await;
    finish_test("todo_lifecycle", r, start)
}

async fn test_pool_messages() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let pool = db.create_pool_session("Test").map_err(backend_err)?;
        let koi = db.create_koi("W", "通用助理", "⚡", "#45b7d1", "Work", "W").map_err(backend_err)?;
        let todo = db.create_koi_todo(&koi.id, "Build", "", "medium", "pisci", Some(&pool.id), "pisci", None).map_err(backend_err)?;
        let m1 = db.insert_pool_message(&pool.id, "pisci", "@W Build", "task_assign", "{}").map_err(backend_err)?;
        let _m2 = db.insert_pool_message_ext(&pool.id, &koi.id, "OK", "task_claimed", "{}", Some(&todo.id), Some(m1.id), Some("task_claimed")).map_err(backend_err)?;
        let msgs = db.get_pool_messages(&pool.id, 100, 0).map_err(backend_err)?;
        if msgs.len() != 2 {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"pool messages","expected":2,"actual":msgs.len()}), format!("expected 2 msgs, got {}", msgs.len())));
        }
        if msgs[1].event_type.as_deref() != Some("task_claimed") {
            return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"event_type","expected":"task_claimed","actual":msgs[1].event_type.clone().unwrap_or_default()}), "event_type mismatch"));

        }
        db.update_pool_org_spec(&pool.id, "## Goal\nBuild").map_err(backend_err)?;
        let u = db.get_pool_session(&pool.id).map_err(backend_err)?.unwrap();
        if !u.org_spec.contains("Build") {
            return Err(fail_with_params("debug.multiAgentErrContains", json!({"subject":"org_spec","expected":"Build"}), "org_spec not saved"));
        }
        ok()
    }.await;
    finish_test("pool_messages", r, start)
}

async fn test_route_to_koi() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let now = Utc::now();
        let kois = vec![
            KoiDefinition {
                id: "fe".into(),
                name: "FE".into(),
                role: "前端工程师".into(),
                icon: "🎨".into(),
                color: "#45b7d1".into(),
                system_prompt: "Frontend React TypeScript CSS UI design expert.".into(),
                description: "Frontend development, React, TypeScript, CSS".into(),
                status: "idle".into(),
                created_at: now,
                updated_at: now,
            },
            KoiDefinition {
                id: "be".into(),
                name: "BE".into(),
                role: "后端工程师".into(),
                icon: "⚡".into(),
                color: "#7c6af7".into(),
                system_prompt: "Backend Rust databases API design expert.".into(),
                description: "Backend development, Rust, database, API".into(),
                status: "idle".into(),
                created_at: now,
                updated_at: now,
            },
            KoiDefinition {
                id: "qa".into(),
                name: "QA".into(),
                role: "测试工程师".into(),
                icon: "🔍".into(),
                color: "#26de81".into(),
                system_prompt: "Testing quality assurance.".into(),
                description: "Testing, QA, automation".into(),
                status: "offline".into(),
                created_at: now,
                updated_at: now,
            },
        ];
        let r = HostAgent::route_to_koi("Create a React component with TypeScript", &kois);
        if r != Some("fe".into()) {
            return Err(fail_with_params(
                "debug.multiAgentErrRoute",
                json!({"subject":"frontend task","actual":format!("{:?}", r),"expected":"fe"}),
                format!("FE routed to {:?}", r),
            ));
        }
        let r = HostAgent::route_to_koi("Design database schema and Rust API", &kois);
        if r != Some("be".into()) {
            return Err(fail_with_params(
                "debug.multiAgentErrRoute",
                json!({"subject":"backend task","actual":format!("{:?}", r),"expected":"be"}),
                format!("BE routed to {:?}", r),
            ));
        }
        let r = HostAgent::route_to_koi("Write tests", &kois);
        if r == Some("qa".into()) {
            return Err(fail(
                "debug.multiAgentErrOfflineRouted",
                "offline Koi routed",
            ));
        }
        if HostAgent::route_to_koi("x", &[]).is_some() {
            return Err(fail_with_params(
                "debug.multiAgentErrUnexpectedSome",
                json!({"subject":"empty route result"}),
                "empty→Some",
            ));
        }
        ok()
    }
    .await;
    finish_test("route_to_koi", r, start)
}

async fn test_runtime_assign_execute() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, bus, runtime) = setup();
        let (koi, pool) = {
            let db = db.lock().await;
            let koi = db
                .create_koi("Builder", "程序员", "🏗️", "#7c6af7", "Build.", "Builder")
                .map_err(backend_err)?;
            let pool = db.create_pool_session("Build").map_err(backend_err)?;
            (koi, pool)
        };
        let result = runtime
            .assign_and_execute(&koi.id, "Implement auth", "pisci", Some(&pool.id), "high")
            .await
            .map_err(backend_err)?;
        if !result.success {
            return Err(fail_with_params(
                "debug.multiAgentErrTaskFailed",
                json!({"subject":"runtime assign execute","detail":result.reply}),
                format!("failed: {}", result.reply),
            ));
        }
        if !result.reply.contains("Builder") {
            return Err(fail_with_params(
                "debug.multiAgentErrContains",
                json!({"subject":"reply","expected":"Builder"}),
                "missing Koi name in reply",
            ));
        }
        {
            let db = db.lock().await;
            let todos = db.list_koi_todos(Some(&koi.id)).map_err(backend_err)?;
            if todos.len() != 1 {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"todos","expected":1,"actual":todos.len()}),
                    format!("todos: {}", todos.len()),
                ));
            }
            if todos[0].status != "done" {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"todo status","expected":"done","actual":todos[0].status}),
                    format!("todo status: {}", todos[0].status),
                ));
            }
            let k = db.get_koi(&koi.id).map_err(backend_err)?.unwrap();
            if k.status != "idle" {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"koi status","expected":"idle","actual":k.status}),
                    format!("koi status: {}", k.status),
                ));
            }
            let msgs = db
                .get_pool_messages(&pool.id, 100, 0)
                .map_err(backend_err)?;
            if msgs.len() < 3 {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedAtLeast",
                    json!({"subject":"pool messages","expected":3,"actual":msgs.len()}),
                    format!("msgs: {}", msgs.len()),
                ));
            }
            let evts: Vec<String> = msgs.iter().filter_map(|m| m.event_type.clone()).collect();
            if !evts.contains(&"task_assigned".into()) {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject":"task_assigned event"}),
                    "no task_assigned",
                ));
            }
            if !evts.contains(&"task_claimed".into()) {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject":"task_claimed event"}),
                    "no task_claimed",
                ));
            }
            if !evts.contains(&"task_completed".into()) {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject":"task_completed event"}),
                    "no task_completed",
                ));
            }
        }
        let events = bus.drain_events().await;
        if !events.iter().any(|(n, _)| n == "koi_status_changed") {
            return Err(fail_with_params(
                "debug.multiAgentErrMissing",
                json!({"subject":"koi_status_changed event"}),
                "no status event",
            ));
        }
        ok()
    }
    .await;
    finish_test("runtime_assign_execute", r, start)
}

async fn test_runtime_mention() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();
        let (koi, pool) = {
            let db = db.lock().await;
            let koi = db
                .create_koi("Rev", "代码审查员", "🔍", "#26de81", "Review.", "Reviewer")
                .map_err(backend_err)?;
            let pool = db.create_pool_session("Review").map_err(backend_err)?;
            (koi, pool)
        };
        let r = runtime
            .handle_mention("pisci", &pool.id, &format!("@{} Review PR", koi.name))
            .await
            .map_err(backend_err)?;
        if r.is_empty() {
            return Err(fail("debug.multiAgentErrShouldMatch", "should match"));
        }
        if !r[0].success {
            return Err(fail("debug.multiAgentErrShouldSucceed", "should succeed"));
        }
        let r = runtime
            .handle_mention("pisci", &pool.id, "@Nobody stuff")
            .await
            .map_err(backend_err)?;
        if !r.is_empty() {
            return Err(fail_with_params(
                "debug.multiAgentErrUnexpectedSome",
                json!({"subject":"unknown mention result"}),
                "should be empty",
            ));
        }
        let r = runtime
            .handle_mention(&koi.id, &pool.id, "@pisci Please review")
            .await
            .map_err(backend_err)?;
        if !r.is_empty() {
            return Err(fail_with_params(
                "debug.multiAgentErrUnexpectedSome",
                json!({"subject":"@pisci runtime mention"}),
                "@pisci should not route through normal Koi mention dispatch",
            ));
        }
        let r = runtime
            .handle_mention("pisci", &pool.id, "no mention")
            .await
            .map_err(backend_err)?;
        if !r.is_empty() {
            return Err(fail_with_params(
                "debug.multiAgentErrUnexpectedSome",
                json!({"subject":"plain text mention result"}),
                "should be empty",
            ));
        }
        ok()
    }
    .await;
    finish_test("runtime_mention", r, start)
}

async fn test_at_all_mention() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();
        let (_fe, _be, qa, pool) = {
            let db = db.lock().await;
            let fe = db
                .create_koi(
                    "FE_All",
                    "前端工程师",
                    "🎨",
                    "#45b7d1",
                    "Frontend.",
                    "FE_All",
                )
                .map_err(backend_err)?;
            let be = db
                .create_koi(
                    "BE_All",
                    "后端工程师",
                    "⚡",
                    "#7c6af7",
                    "Backend.",
                    "BE_All",
                )
                .map_err(backend_err)?;
            let qa = db
                .create_koi(
                    "QA_All",
                    "测试工程师",
                    "🔍",
                    "#26de81",
                    "Testing.",
                    "QA_All",
                )
                .map_err(backend_err)?;
            let pool = db.create_pool_session("AtAllTest").map_err(backend_err)?;
            db.update_koi_status(&qa.id, "offline")
                .map_err(backend_err)?;
            (fe, be, qa, pool)
        };

        let results = runtime
            .handle_mention("pisci", &pool.id, "大家好，@all 请参加讨论")
            .await
            .map_err(backend_err)?;

        // Should activate FE_All and BE_All (both idle), but not QA_All (offline)
        if results.len() != 2 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject": "@all activated count", "expected": 2, "actual": results.len()}),
                format!("expected 2 koi activated, got {}", results.len()),
            ));
        }
        for r in &results {
            if !r.success {
                return Err(fail_with_params(
                    "debug.multiAgentErrTaskFailed",
                    json!({"subject": "@all activation", "detail": r.reply}),
                    format!("@all activation failed: {}", r.reply),
                ));
            }
        }

        // Verify offline QA was NOT activated (no todo created for it)
        {
            let db = db.lock().await;
            let qa_todos = db.list_koi_todos(Some(&qa.id)).map_err(backend_err)?;
            if !qa_todos.is_empty() {
                return Err(fail_with_params(
                    "debug.multiAgentErrUnexpectedSome",
                    json!({"subject": "offline QA todos"}),
                    "offline QA should have no todos",
                ));
            }
        }

        // Verify the activated Koi have results
        let activated_names: Vec<&str> = results.iter().map(|r| r.reply.as_str()).collect();
        let has_fe = activated_names.iter().any(|r| r.contains("FE_All"));
        let has_be = activated_names.iter().any(|r| r.contains("BE_All"));
        if !has_fe || !has_be {
            return Err(fail_with_params(
                "debug.multiAgentErrMissing",
                json!({"subject": format!("FE_All={}, BE_All={}", has_fe, has_be)}),
                format!("not all idle koi activated: FE={} BE={}", has_fe, has_be),
            ));
        }

        ok()
    }
    .await;
    finish_test("at_all_mention", r, start)
}

async fn test_pool_chat_conversation() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, bus, runtime) = setup();
        let (_pisci_koi, arch, dev, pool) = {
            let db = db.lock().await;
            let arch = db.create_koi("Architect", "架构师", "🏛️", "#f7b731", "Architecture design.", "Architect")
                .map_err(backend_err)?;
            let dev = db.create_koi("Developer", "开发者", "💻", "#45b7d1", "Full-stack development.", "Developer")
                .map_err(backend_err)?;
            let pool = db.create_pool_session("OpenClawLobster").map_err(backend_err)?;
            ((), arch, dev, pool)
        };

        // Step 1: Pisci posts a discussion topic with @all
        let topic = "最近OpenClaw龙虾比较火，大家怎么看，@all";
        {
            let db = db.lock().await;
            db.insert_pool_message(&pool.id, "pisci", topic, "text", "{}")
                .map_err(backend_err)?;
        }

        // Step 2: @all should activate all idle Koi
        let mention_results = runtime.handle_mention("pisci", &pool.id, topic)
            .await.map_err(backend_err)?;

        if mention_results.len() != 2 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject": "conversation @all count", "expected": 2, "actual": mention_results.len()}),
                format!("expected 2, got {}", mention_results.len()),
            ));
        }
        for r in &mention_results {
            if !r.success {
                return Err(fail_with_params(
                    "debug.multiAgentErrTaskFailed",
                    json!({"subject": "conversation activation", "detail": r.reply}),
                    format!("activation failed: {}", r.reply),
                ));
            }
        }

        // Step 3: Simulate Koi replies in the pool chat
        {
            let db = db.lock().await;
            db.insert_pool_message(
                &pool.id, &arch.id,
                "OpenClaw龙虾的分布式架构很有趣，值得研究下微服务拆分方案。",
                "text", "{}",
            ).map_err(backend_err)?;
            db.insert_pool_message(
                &pool.id, &dev.id,
                "同意，我可以先做个原型验证技术可行性。@Architect 你觉得用Rust还是Go？",
                "text", "{}",
            ).map_err(backend_err)?;
            db.insert_pool_message(
                &pool.id, &arch.id,
                "[ProjectStatus] ready_for_pisci_review @pisci 讨论已经收敛，可以由 Pisci 判断是否结束。",
                "text", "{}",
            ).map_err(backend_err)?;
        }

        // Step 4: Verify conversation flow — messages are in the pool
        {
            let db = db.lock().await;
            let msgs = db.get_pool_messages(&pool.id, 100, 0).map_err(backend_err)?;
            // 1 topic + 2 Koi replies = 3 minimum (runtime may post task_assigned etc.)
            if msgs.len() < 3 {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedAtLeast",
                    json!({"subject": "conversation messages", "expected": 3, "actual": msgs.len()}),
                    format!("expected ≥3 msgs, got {}", msgs.len()),
                ));
            }

            let has_topic = msgs.iter().any(|m| m.content.contains("OpenClaw"));
            let has_arch_reply = msgs.iter().any(|m| m.sender_id == arch.id);
            let has_dev_reply = msgs.iter().any(|m| m.sender_id == dev.id);
            if !has_topic {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject": "topic message"}),
                    "topic message not found in pool",
                ));
            }
            if !has_arch_reply {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject": "Architect reply"}),
                    "Architect reply not in pool",
                ));
            }
            if !has_dev_reply {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject": "Developer reply"}),
                    "Developer reply not in pool",
                ));
            }
            let assessment = assess_trial_project_state(&msgs, &[], &vec![arch.id.clone(), dev.id.clone()]);
            if assessment.decision != TrialDecision::ReadyForPisciReview {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"conversation handoff to pisci","expected":"ReadyForPisciReview","actual":format!("{:?}", assessment.decision)}),
                    "conversation should end by explicitly handing off to Pisci",
                ));
            }
        }

        // Step 5: Developer @mentions Architect — Koi-to-Koi peer mention
        let peer_mention_results = runtime.handle_mention(
            &dev.id, &pool.id,
            "@Architect 你觉得用Rust还是Go？",
        ).await.map_err(backend_err)?;

        // Architect should be activated (idle) or notified (busy)
        if peer_mention_results.is_empty() {
            return Err(fail(
                "debug.multiAgentErrShouldMatch",
                "peer @mention should activate Architect",
            ));
        }
        if !peer_mention_results[0].success {
            return Err(fail_with_params(
                "debug.multiAgentErrTaskFailed",
                json!({"subject": "peer mention", "detail": peer_mention_results[0].reply}),
                "peer mention failed",
            ));
        }

        // Step 6: All Koi should be idle after conversation ends
        {
            let db = db.lock().await;
            for kid in [&arch.id, &dev.id] {
                let k = db.get_koi(kid).map_err(backend_err)?.unwrap();
                if k.status != "idle" {
                    return Err(fail_with_params(
                        "debug.multiAgentErrKoiNotIdle",
                        json!({"subject": k.name}),
                        format!("{} not idle after conversation", k.name),
                    ));
                }
            }
        }

        // Step 7: Verify EventBus received status events
        let events = bus.drain_events().await;
        let status_changes = events.iter().filter(|(n, _)| n == "koi_status_changed").count();
        if status_changes < 4 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedAtLeast",
                json!({"subject": "conversation status changes", "expected": 4, "actual": status_changes}),
                format!("status changes: {} (expected ≥4)", status_changes),
            ));
        }

        // Step 8: Cleanup — delete pool (simulating temp project)
        {
            let db = db.lock().await;
            db.conn.execute("DELETE FROM pool_messages WHERE pool_session_id = ?1", rusqlite::params![pool.id])
                .map_err(backend_err)?;
            db.conn.execute("DELETE FROM pool_sessions WHERE id = ?1", rusqlite::params![pool.id])
                .map_err(backend_err)?;
            let check = db.get_pool_session(&pool.id).map_err(backend_err)?;
            if check.is_some() {
                return Err(fail_with_params(
                    "debug.multiAgentErrUnexpectedSome",
                    json!({"subject": "deleted pool"}),
                    "temp pool should be deleted",
                ));
            }
        }

        ok()
    }.await;
    finish_test("pool_chat_conversation", r, start)
}

async fn test_full_e2e() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, bus, runtime) = setup();
        let (fe, be, qa, pool) = {
            let db = db.lock().await;
            let fe = db.create_koi("FE", "前端工程师", "🎨", "#45b7d1", "Frontend React.", "FE").map_err(backend_err)?;
            let be = db.create_koi("BE", "后端工程师", "⚡", "#7c6af7", "Backend Rust API.", "BE").map_err(backend_err)?;
            let qa = db.create_koi("QA", "测试工程师", "🔍", "#26de81", "Testing QA.", "QA").map_err(backend_err)?;
            let pool = db.create_pool_session("E-Commerce").map_err(backend_err)?;
            db.update_pool_org_spec(&pool.id, "## Goal\nBuild e-commerce MVP").map_err(backend_err)?;
            (fe, be, qa, pool)
        };
        let r1 = runtime.assign_and_execute(&be.id, "Build Product API", "pisci", Some(&pool.id), "high")
            .await.map_err(backend_err)?;
        if !r1.success {
            return Err(fail_with_params("debug.multiAgentErrTaskFailed", json!({"subject":"BE","detail":r1.reply}), format!("BE failed: {}", r1.reply)));
        }
        let r2 = runtime.assign_and_execute(&fe.id, "Build product listing", "pisci", Some(&pool.id), "medium")
            .await.map_err(backend_err)?;
        if !r2.success {
            return Err(fail_with_params("debug.multiAgentErrTaskFailed", json!({"subject":"FE","detail":r2.reply}), format!("FE failed: {}", r2.reply)));
        }
        let r3 = runtime.handle_mention(&be.id, &pool.id, &format!("@{} Review API", qa.name))
            .await.map_err(backend_err)?;
        if r3.is_empty() || !r3[0].success {
            return Err(fail("debug.multiAgentErrQaMentionFailed", "QA mention failed"));
        }
        {
            let db = db.lock().await;
            db.insert_pool_message(
                &pool.id,
                &qa.id,
                "[ProjectStatus] ready_for_pisci_review @pisci Backend, frontend, and QA checks are aligned.",
                "text",
                "{}",
            ).map_err(backend_err)?;
            let todos = db.list_koi_todos(None).map_err(backend_err)?;
            // BE and FE each get a todo via assign_and_execute.
            // QA is activated via Koi-to-Koi @mention (activate_for_messages), which does NOT create a todo.
            if todos.len() != 2 {
                return Err(fail_with_params("debug.multiAgentErrExpectedActual", json!({"subject":"all todos","expected":2,"actual":todos.len()}), format!("todos: {}", todos.len())));
            }
            for t in &todos {
                if t.status != "done" {
                    return Err(fail_with_params("debug.multiAgentErrTodoStatus", json!({"subject":t.title,"actual":t.status,"expected":"done"}), format!("'{}' status: {}", t.title, t.status)));
                }
            }
            let msgs = db.get_pool_messages(&pool.id, 100, 0).map_err(backend_err)?;
            // BE: task_assigned + task_claimed + task_completed = 3
            // FE: task_assigned + task_claimed + task_completed = 3
            // QA: activated via peer @mention (activate_for_messages), no pool messages in test mode
            if msgs.len() < 6 {
                return Err(fail_with_params("debug.multiAgentErrExpectedAtLeast", json!({"subject":"e2e messages","expected":6,"actual":msgs.len()}), format!("msgs: {} (expected ≥6)", msgs.len())));
            }
            let assessment = assess_trial_project_state(&msgs, &todos, &vec![fe.id.clone(), be.id.clone(), qa.id.clone()]);
            if assessment.decision != TrialDecision::ReadyForPisciReview {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"e2e project completion","expected":"ReadyForPisciReview","actual":format!("{:?}", assessment.decision)}),
                    "full e2e flow should hand the final decision back to Pisci",
                ));
            }
            for kid in [&fe.id, &be.id, &qa.id] {
                let k = db.get_koi(kid).map_err(backend_err)?.unwrap();
                if k.status != "idle" {
                    return Err(fail_with_params("debug.multiAgentErrKoiNotIdle", json!({"subject":k.name}), format!("{} not idle", k.name)));
                }
            }
        }
        let events = bus.drain_events().await;
        let sc = events.iter().filter(|(n, _)| n == "koi_status_changed").count();
        if sc < 6 {
            return Err(fail_with_params("debug.multiAgentErrExpectedAtLeast", json!({"subject":"status changes","expected":6,"actual":sc}), format!("status changes: {} (expected ≥6)", sc)));
        }
        ok()
    }.await;
    finish_test("full_e2e_collaboration", r, start)
}

async fn test_koi_limit() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        for i in 0..5 {
            db.create_koi(
                &format!("K{}", i),
                "worker",
                "🐟",
                "#45b7d1",
                &format!("Worker {}.", i),
                &format!("K{}", i),
            )
            .map_err(backend_err)?;
        }
        let count = db.list_kois().map_err(backend_err)?.len();
        if count != 5 {
            return Err(fail_with_params(
                "debug.multiAgentErrKoiLimit",
                json!({"expected": 5, "actual": count}),
                format!("expected 5 kois, got {}", count),
            ));
        }
        ok()
    }
    .await;
    finish_test("koi_limit", r, start)
}

async fn test_vacation_cancels_todos() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let koi = db
            .create_koi(
                "Vacationer",
                "通用助理",
                "🏖️",
                "#f7b731",
                "Work.",
                "Vacationer",
            )
            .map_err(backend_err)?;
        let _t1 = db
            .create_koi_todo(
                &koi.id, "Task A", "", "medium", "pisci", None, "pisci", None,
            )
            .map_err(backend_err)?;
        let t2 = db
            .create_koi_todo(&koi.id, "Task B", "", "high", "pisci", None, "pisci", None)
            .map_err(backend_err)?;
        db.claim_koi_todo(&t2.id, &koi.id).map_err(backend_err)?;

        // Simulate vacation: set offline and cancel uncompleted todos
        db.update_koi_status(&koi.id, "offline")
            .map_err(backend_err)?;
        let todos = db.list_koi_todos(Some(&koi.id)).map_err(backend_err)?;
        for todo in &todos {
            if todo.status == "todo" || todo.status == "in_progress" {
                let _ = db.update_koi_todo(&todo.id, None, None, Some("cancelled"), None);
            }
        }

        let updated = db.list_koi_todos(Some(&koi.id)).map_err(backend_err)?;
        for t in &updated {
            if t.status != "cancelled" {
                return Err(fail_with_params(
                    "debug.multiAgentErrVacation",
                    json!({"subject": t.title, "actual": t.status}),
                    format!("'{}' not cancelled: {}", t.title, t.status),
                ));
            }
        }

        let k = db.get_koi(&koi.id).map_err(backend_err)?.unwrap();
        if k.status != "offline" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"koi status","expected":"offline","actual":k.status}),
                "koi should be offline",
            ));
        }
        ok()
    }
    .await;
    finish_test("vacation_cancels_todos", r, start)
}

async fn test_watchdog_recover() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();
        {
            let db = db.lock().await;
            let koi = db
                .create_koi("StaleKoi", "worker", "⏰", "#fc5c65", "Work.", "StaleKoi")
                .map_err(backend_err)?;
            db.update_koi_status(&koi.id, "busy").map_err(backend_err)?;
            // Backdate updated_at to simulate a stale entry (30 min ago)
            db.conn
                .execute(
                    "UPDATE kois SET updated_at = datetime('now', '-30 minutes') WHERE id = ?1",
                    rusqlite::params![koi.id],
                )
                .map_err(backend_err)?;

            let todo = db
                .create_koi_todo(
                    &koi.id,
                    "Stale task",
                    "",
                    "medium",
                    "pisci",
                    None,
                    "pisci",
                    None,
                )
                .map_err(backend_err)?;
            db.claim_koi_todo(&todo.id, &koi.id).map_err(backend_err)?;
            db.conn.execute(
                "UPDATE koi_todos SET updated_at = datetime('now', '-30 minutes') WHERE id = ?1",
                rusqlite::params![todo.id],
            ).map_err(backend_err)?;
        }

        let (koi_recovered, todo_recovered) = runtime.watchdog_recover(600).await;
        if koi_recovered == 0 {
            return Err(fail_with_params(
                "debug.multiAgentErrWatchdog",
                json!({"subject":"koi_recovered","expected":">0","actual":koi_recovered}),
                "no koi recovered",
            ));
        }
        if todo_recovered == 0 {
            return Err(fail_with_params(
                "debug.multiAgentErrWatchdog",
                json!({"subject":"todo_recovered","expected":">0","actual":todo_recovered}),
                "no todo recovered",
            ));
        }

        {
            let db = db.lock().await;
            let kois = db.list_kois().map_err(backend_err)?;
            for k in &kois {
                if k.status == "busy" {
                    return Err(fail_with_params(
                        "debug.multiAgentErrWatchdog",
                        json!({"subject":k.name,"expected":"idle","actual":"busy"}),
                        format!("{} still busy after watchdog", k.name),
                    ));
                }
            }
            let todos = db.list_koi_todos(None).map_err(backend_err)?;
            for t in &todos {
                if t.status == "in_progress" {
                    return Err(fail_with_params(
                        "debug.multiAgentErrWatchdog",
                        json!({"subject":t.title,"expected":"todo","actual":"in_progress"}),
                        format!("'{}' still in_progress", t.title),
                    ));
                }
            }
        }
        ok()
    }
    .await;
    finish_test("watchdog_recover", r, start)
}

async fn test_activate_pending_todos_no_duplicates() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();
        let koi_id;
        {
            let db = db.lock().await;
            let koi = db.create_koi("PatrolKoi", "worker", "🧭", "#5b8def", "Work.", "PatrolKoi")
                .map_err(backend_err)?;
            koi_id = koi.id.clone();
            let _todo = db.create_koi_todo(&koi.id, "Resume existing todo", "", "medium", "pisci", None, "pisci", None)
                .map_err(backend_err)?;
        }

        let activated = runtime.activate_pending_todos(None).await.map_err(backend_err)?;
        if activated != 1 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"activated pending todos","expected":1,"actual":activated}),
                format!("expected 1 activated todo, got {}", activated),
            ));
        }

        {
            let db = db.lock().await;
            let todos = db.list_koi_todos(Some(&koi_id)).map_err(backend_err)?;
            if todos.len() != 1 {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"todo count after patrol activation","expected":1,"actual":todos.len()}),
                    format!("expected patrol activation to reuse the existing todo, got {}", todos.len()),
                ));
            }
            let todo = &todos[0];
            if todo.status != "done" {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"pending todo status after patrol activation","expected":"done","actual":todo.status}),
                    format!("expected resumed todo to complete, got {}", todo.status),
                ));
            }
            if todo.claimed_by.as_deref() != Some(&koi_id) {
                return Err(fail_with_params(
                    "debug.multiAgentErrExpectedActual",
                    json!({"subject":"claimed_by after patrol activation","expected":koi_id,"actual":todo.claimed_by}),
                    "pending todo was not claimed by its owner during patrol activation",
                ));
            }
        }

        ok()
    }.await;
    finish_test("activate_pending_todos_no_duplicates", r, start)
}

async fn test_starter_koi_seed() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;

        let created = db.ensure_starter_kois().map_err(backend_err)?;
        if created.len() != 3 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"starter koi count","expected":3,"actual":created.len()}),
                format!("expected 3 starter kois, got {}", created.len()),
            ));
        }

        let all = db.list_kois().map_err(backend_err)?;
        for expected in ["Architect", "Coder", "Reviewer"] {
            if !all.iter().any(|k| k.name == expected) {
                return Err(fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject": expected}),
                    format!("starter koi '{}' missing", expected),
                ));
            }
        }

        let second_run = db.ensure_starter_kois().map_err(backend_err)?;
        if !second_run.is_empty() {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"starter koi reseed count","expected":0,"actual":second_run.len()}),
                "starter koi seeding should be idempotent once koi already exist",
            ));
        }

        ok()
    }.await;
    finish_test("starter_koi_seed", r, start)
}

async fn test_watchdog_recover_zero_threshold() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();
        let koi_id;
        let todo_id;
        {
            let db = db.lock().await;
            let koi = db.create_koi("ZeroWatchdog", "worker", "⏱", "#7c4dff", "Recover immediately.", "ZeroWatchdog")
                .map_err(backend_err)?;
            koi_id = koi.id.clone();
            db.update_koi_status(&koi_id, "busy").map_err(backend_err)?;
            let todo = db.create_koi_todo(&koi_id, "Immediate recover", "", "medium", "pisci", None, "pisci", None)
                .map_err(backend_err)?;
            todo_id = todo.id.clone();
            db.claim_koi_todo(&todo_id, &koi_id).map_err(backend_err)?;
        }

        let (koi_recovered, todo_recovered) = runtime.watchdog_recover(0).await;
        if koi_recovered < 1 || todo_recovered < 1 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"watchdog zero-threshold recovery","expected":"koi>=1,todo>=1","actual":format!("koi={}, todo={}", koi_recovered, todo_recovered)}),
                format!("expected zero-threshold watchdog recovery, got koi={} todo={}", koi_recovered, todo_recovered),
            ));
        }

        let db = db.lock().await;
        let koi = db.get_koi(&koi_id).map_err(backend_err)?
            .ok_or_else(|| fail_with_params("debug.multiAgentErrMissing", json!({"subject":"koi"}), "koi missing after watchdog"))?;
        if koi.status != "idle" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"koi status after zero-threshold watchdog","expected":"idle","actual":koi.status}),
                "zero-threshold watchdog did not reset busy koi",
            ));
        }
        let todo = db.get_koi_todo(&todo_id).map_err(backend_err)?
            .ok_or_else(|| fail_with_params("debug.multiAgentErrMissing", json!({"subject":"todo"}), "todo missing after watchdog"))?;
        if todo.status != "todo" || todo.claimed_by.is_some() {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"todo recovery after zero-threshold watchdog","expected":"status=todo, claimed_by=None","actual":{"status":todo.status,"claimed_by":todo.claimed_by}}),
                "zero-threshold watchdog did not reset in-progress todo",
            ));
        }

        ok()
    }.await;
    finish_test("watchdog_recover_zero_threshold", r, start)
}

async fn test_pool_project_dir() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let pool = db.create_pool_session_with_dir("DirProject", Some("C:\\Projects\\test-proj"))
            .map_err(backend_err)?;
        if pool.project_dir.as_deref() != Some("C:\\Projects\\test-proj") {
            return Err(fail_with_params(
                "debug.multiAgentErrProjectDir",
                json!({"expected":"C:\\Projects\\test-proj","actual":format!("{:?}", pool.project_dir)}),
                "project_dir mismatch on create",
            ));
        }
        let fetched = db.get_pool_session(&pool.id).map_err(backend_err)?
            .ok_or_else(|| fail_with_params("debug.multiAgentErrMissing", json!({"subject":"pool"}), "pool not found"))?;
        if fetched.project_dir.as_deref() != Some("C:\\Projects\\test-proj") {
            return Err(fail_with_params(
                "debug.multiAgentErrProjectDir",
                json!({"expected":"C:\\Projects\\test-proj","actual":format!("{:?}", fetched.project_dir)}),
                "project_dir mismatch on fetch",
            ));
        }
        let listed = db.list_pool_sessions().map_err(backend_err)?;
        let found = listed.iter().find(|s| s.id == pool.id);
        if found.and_then(|s| s.project_dir.as_deref()) != Some("C:\\Projects\\test-proj") {
            return Err(fail_with_params(
                "debug.multiAgentErrProjectDir",
                json!({"expected":"C:\\Projects\\test-proj","actual":"not found in list"}),
                "project_dir missing in list",
            ));
        }

        let pool_no_dir = db.create_pool_session("NoDirProject").map_err(backend_err)?;
        if pool_no_dir.project_dir.is_some() {
            return Err(fail_with_params(
                "debug.multiAgentErrProjectDir",
                json!({"expected":"None","actual":format!("{:?}", pool_no_dir.project_dir)}),
                "project_dir should be None",
            ));
        }
        ok()
    }.await;
    finish_test("pool_project_dir", r, start)
}

async fn test_pool_session_prefix_lookup() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let pool = db
            .create_pool_session("PrefixProject")
            .map_err(backend_err)?;
        let prefix = &pool.id[..8.min(pool.id.len())];
        let resolved = db
            .get_pool_session_by_prefix(prefix)
            .map_err(backend_err)?
            .ok_or_else(|| {
                fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject":"pool prefix lookup"}),
                    "pool prefix lookup returned none",
                )
            })?;
        if resolved.id != pool.id {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"pool prefix lookup","expected":pool.id,"actual":resolved.id}),
                "pool prefix lookup resolved the wrong pool",
            ));
        }
        let by_name = db
            .resolve_pool_session_identifier(&pool.name)
            .map_err(backend_err)?
            .ok_or_else(|| {
                fail_with_params(
                    "debug.multiAgentErrMissing",
                    json!({"subject":"pool name lookup"}),
                    "pool name lookup returned none",
                )
            })?;
        if by_name.id != pool.id {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"pool name lookup","expected":pool.id,"actual":by_name.id}),
                "pool name lookup resolved the wrong pool",
            ));
        }
        ok()
    }
    .await;
    finish_test("pool_session_prefix_lookup", r, start)
}

async fn test_headless_session_scope_isolation() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        crate::commands::chat::validate_headless_session_scope(
            crate::commands::chat::SESSION_SOURCE_PISCI_INBOX_POOL,
            crate::commands::chat::SESSION_SOURCE_PISCI_INBOX_POOL,
            Some("pool-1"),
        ).map_err(backend_err)?;

        if crate::commands::chat::validate_headless_session_scope(
            "im_feishu",
            crate::commands::chat::SESSION_SOURCE_PISCI_INBOX_POOL,
            Some("pool-1"),
        ).is_ok() {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"pool/internal session isolation","expected":"error","actual":"ok"}),
                "pool-scoped runs must not reuse IM sessions",
            ));
        }

        if crate::commands::chat::validate_headless_session_scope(
            crate::commands::chat::SESSION_SOURCE_PISCI_INBOX_POOL,
            crate::commands::chat::SESSION_SOURCE_PISCI_INTERNAL,
            None,
        ).is_ok() {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"non-pool session isolation","expected":"error","actual":"ok"}),
                "non-pool runs must not reuse pool-scoped sessions",
            ));
        }

        ok()
    }.await;
    finish_test("headless_session_scope_isolation", r, start)
}

async fn test_runtime_identifier_canonicalization() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();
        let (koi, pool, prefix) = {
            let db = db.lock().await;
            let koi = db.create_koi("CanonicalWorker", "worker", "🧪", "#4b7bec", "Work.", "CanonicalWorker")
                .map_err(backend_err)?;
            let pool = db.create_pool_session("Canonical Project").map_err(backend_err)?;
            let prefix = koi.id[..8.min(koi.id.len())].to_string();
            (koi, pool, prefix)
        };

        let (todo, assign_msg_id) = runtime.assign_task(
            &prefix,
            "Use canonical identifiers",
            "pisci",
            Some(&pool.name),
            "medium",
        ).await.map_err(backend_err)?;

        if todo.owner_id != koi.id {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"todo owner canonicalization","expected":koi.id,"actual":todo.owner_id}),
                "runtime.assign_task should store the canonical koi id",
            ));
        }
        if todo.pool_session_id.as_deref() != Some(pool.id.as_str()) {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"todo pool canonicalization","expected":pool.id,"actual":todo.pool_session_id}),
                "runtime.assign_task should store the canonical pool id",
            ));
        }

        runtime.execute_todo(&prefix, &todo, assign_msg_id, Some(&pool.name))
            .await
            .map_err(backend_err)?;

        let db = db.lock().await;
        let refreshed = db.get_koi_todo(&todo.id).map_err(backend_err)?
            .ok_or_else(|| fail_with_params("debug.multiAgentErrMissing", json!({"subject":"canonicalized todo"}), "todo missing after execution"))?;
        if refreshed.claimed_by.as_deref() != Some(koi.id.as_str()) {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"todo claimed_by canonicalization","expected":koi.id,"actual":refreshed.claimed_by}),
                "runtime.execute_todo should claim with the canonical koi id",
            ));
        }
        if refreshed.status != "done" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"todo status after canonical execution","expected":"done","actual":refreshed.status}),
                "canonicalized execution should still complete the todo",
            ));
        }
        ok()
    }.await;
    finish_test("runtime_identifier_canonicalization", r, start)
}

async fn test_recover_stale_running_sessions() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, _) = setup();
        let db = db.lock().await;
        let session_id = "im_test_stale_session";
        db.ensure_im_session(session_id, "Stale Session", "heartbeat").map_err(backend_err)?;
        db.update_session_status(session_id, "running").map_err(backend_err)?;
        let recovered = db.recover_stale_running_sessions(0).map_err(backend_err)?;
        if recovered < 1 {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"stale running session recovery","expected":"at least 1","actual":recovered}),
                "expected running session recovery to reset the session",
            ));
        }
        let session = db.list_sessions(20, 0).map_err(backend_err)?
            .into_iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| fail_with_params("debug.multiAgentErrMissing", json!({"subject":"session"}), "session missing after recovery"))?;
        if session.status != "idle" {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"session status after stale recovery","expected":"idle","actual":session.status}),
                "stale running session was not reset to idle",
            ));
        }
        ok()
    }.await;
    finish_test("recover_stale_running_sessions", r, start)
}

async fn test_archived_pool_blocks_runtime_work() -> TestResult {
    let start = std::time::Instant::now();
    let r: Result<(), LocalizedError> = async {
        let (db, _, runtime) = setup();

        let (koi, pool) = {
            let db = db.lock().await;
            let koi = db.create_koi("Archivist", "Archive guard", "🗄️", "#888888", "Protect archived pools.", "Archive guard")
                .map_err(backend_err)?;
            let pool = db.create_pool_session("Archive Guard Pool").map_err(backend_err)?;
            db.update_pool_session_status(&pool.id, "archived").map_err(backend_err)?;
            (koi, pool)
        };

        let assign_err = match runtime.assign_task(
            &koi.id,
            "This should be rejected",
            "pisci",
            Some(&pool.id),
            "medium",
        ).await {
            Ok(_) => {
                return Err(fail(
                    "debug.multiAgentErrUnexpectedSome",
                    "archived pool unexpectedly accepted a runtime assignment",
                ));
            }
            Err(err) => err,
        };
        if !assign_err.to_string().contains("cannot accept new task assignments") {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"archived assign_task error","actual":assign_err.to_string()}),
                "archived pool did not reject task assignment with the expected reason",
            ));
        }

        let activate_err = match runtime.activate_for_messages(&koi.id, &pool.id).await {
            Ok(_) => {
                return Err(fail(
                    "debug.multiAgentErrUnexpectedSome",
                    "archived pool unexpectedly activated a Koi from a mention",
                ));
            }
            Err(err) => err,
        };
        if !activate_err.to_string().contains("cannot activate Koi message handling") {
            return Err(fail_with_params(
                "debug.multiAgentErrExpectedActual",
                json!({"subject":"archived activate_for_messages error","actual":activate_err.to_string()}),
                "archived pool did not reject mention activation with the expected reason",
            ));
        }

        ok()
    }.await;
    finish_test("archived_pool_blocks_runtime_work", r, start)
}
