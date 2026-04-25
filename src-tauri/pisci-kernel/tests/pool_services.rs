//! Integration tests for `pisci_kernel::pool::services`.
//!
//! These tests exercise the kernel pool layer end-to-end against a real
//! in-memory SQLite `Database`, a capturing `PoolEventSink`, and the
//! in-process [`StubSubagentRuntime`]. They intentionally do NOT spin
//! up an LLM, Tauri app handle, or filesystem worktree — the only side
//! effect under test is `(DB rows, emitted events)`.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use pisci_core::host::{KoiTurnRequest, PoolEvent, PoolEventSink};
use pisci_kernel::pool::{
    self, coordinator::CoordinatorConfig, services, store::PoolStore, AssignKoiArgs, CallerContext,
    CreatePoolArgs, CreateTodoArgs, DeleteTodoArgs, SendPoolMessageArgs, StubOutcome,
    StubSubagentRuntime,
};
use pisci_kernel::store::Database;
use tokio::sync::Mutex;

// ─── test plumbing ─────────────────────────────────────────────────────

#[derive(Default)]
struct CapturingSink {
    events: StdMutex<Vec<PoolEvent>>,
}

impl CapturingSink {
    fn drain_kinds(&self) -> Vec<&'static str> {
        let events = self.events.lock().unwrap();
        events.iter().map(|e| e.kind()).collect()
    }

    fn count(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

impl PoolEventSink for CapturingSink {
    fn emit_pool(&self, event: &PoolEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn pisci_caller<'a>(session_id: &'a str) -> CallerContext<'a> {
    CallerContext {
        memory_owner_id: "pisci",
        session_id,
        session_source: Some("test"),
        pool_session_id: None,
        cancel: None,
    }
}

fn koi_caller<'a>(koi: &'a str, pool_id: &'a str) -> CallerContext<'a> {
    CallerContext {
        memory_owner_id: koi,
        session_id: koi,
        session_source: Some("test"),
        pool_session_id: Some(pool_id),
        cancel: None,
    }
}

fn build_store() -> PoolStore {
    let db = Database::open_in_memory().expect("open in-memory db");
    // Satisfy `koi_todos.owner_id`/`claimed_by` FK constraints for every
    // caller used in the suite. Production hosts seed Pisci via
    // `ensure_starter_kois`; tests insert them explicitly so they can
    // use stable literal ids.
    for (id, name) in [
        ("pisci", "Pisci"),
        ("koi-alpha", "Alpha"),
        ("koi-beta", "Beta"),
    ] {
        db.upsert_koi_with_id(id, name).expect("seed koi");
    }
    PoolStore::new(Arc::new(Mutex::new(db)))
}

async fn create_test_pool(store: &PoolStore, sink: &Arc<CapturingSink>) -> String {
    let caller = pisci_caller("sess-1");
    let value = services::create_pool(
        store,
        sink.as_ref(),
        &caller,
        CreatePoolArgs {
            name: "Integration Pool".into(),
            project_dir: None,
            org_spec: Some("be nice".into()),
            task_timeout_secs: 0,
            origin_im_binding_key: None,
        },
    )
    .await
    .expect("create_pool");
    value["pool"]["id"].as_str().unwrap().to_string()
}

// ─── actual tests ──────────────────────────────────────────────────────

fn make_sink() -> Arc<CapturingSink> {
    Arc::new(CapturingSink::default())
}

fn sink_arc(sink: &Arc<CapturingSink>) -> Arc<dyn PoolEventSink> {
    sink.clone() as Arc<dyn PoolEventSink>
}

#[tokio::test]
async fn create_pool_emits_pool_created_and_welcome_message() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;
    assert!(!pool_id.is_empty());

    let kinds = sink.drain_kinds();
    assert_eq!(
        kinds,
        vec!["pool_created", "message_appended"],
        "create_pool must emit exactly PoolCreated + MessageAppended"
    );
}

#[tokio::test]
async fn create_pool_persists_origin_im_binding_key_when_provided() {
    let store = build_store();
    let sink = make_sink();
    let caller = pisci_caller("sess-im");
    let value = services::create_pool(
        &store,
        sink.as_ref(),
        &caller,
        CreatePoolArgs {
            name: "IM-originated pool".into(),
            project_dir: None,
            org_spec: None,
            task_timeout_secs: 0,
            origin_im_binding_key: Some("wechat::dm:user-1".into()),
        },
    )
    .await
    .expect("create_pool");
    let pool_id = value["pool"]["id"].as_str().unwrap().to_string();

    let pool = store
        .read(move |db| db.get_pool_session(&pool_id))
        .await
        .expect("read pool")
        .expect("pool exists");
    assert_eq!(
        pool.origin_im_binding_key.as_deref(),
        Some("wechat::dm:user-1")
    );
}

#[tokio::test]
async fn create_pool_leaves_origin_im_binding_key_none_for_desktop_callers() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;

    let pool = store
        .read(move |db| db.get_pool_session(&pool_id))
        .await
        .expect("read pool")
        .expect("pool exists");
    assert!(pool.origin_im_binding_key.is_none());
}

#[tokio::test]
async fn send_pool_message_emits_message_appended() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;

    let caller = pisci_caller("sess-1");
    let before = sink.count();
    let cfg = CoordinatorConfig::default();
    let msg = services::send_pool_message(
        &store,
        sink_arc(&sink),
        None,
        &cfg,
        &caller,
        SendPoolMessageArgs {
            pool_id: pool_id.clone(),
            sender_id: "pisci".into(),
            content: "hello pool".into(),
            reply_to_message_id: None,
        },
    )
    .await
    .expect("send_pool_message");
    assert_eq!(msg.pool_session_id, pool_id);
    assert_eq!(msg.content, "hello pool");

    let all = sink.events.lock().unwrap();
    assert_eq!(all.len(), before + 1, "exactly one new event expected");
    match &all[before] {
        PoolEvent::MessageAppended {
            pool_id: pid,
            message,
        } => {
            assert_eq!(pid, &pool_id);
            assert_eq!(message.content, "hello pool");
        }
        other => panic!("expected MessageAppended, got {:?}", other.kind()),
    }
}

#[tokio::test]
async fn set_pool_status_pause_then_resume() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;

    let caller = pisci_caller("sess-1");
    services::set_pool_status(&store, sink.as_ref(), &caller, &pool_id, "paused")
        .await
        .expect("pause");
    services::set_pool_status(&store, sink.as_ref(), &caller, &pool_id, "active")
        .await
        .expect("resume");

    let kinds = sink.drain_kinds();
    // create_pool + welcome + (pool_paused + status_msg) + (pool_resumed + status_msg)
    assert!(
        kinds.contains(&"pool_paused"),
        "expected pool_paused event in {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"pool_resumed"),
        "expected pool_resumed event in {:?}",
        kinds
    );
}

#[tokio::test]
async fn archive_rejects_pool_with_active_todos() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;

    let caller = pisci_caller("sess-1");
    services::create_todo(
        &store,
        sink.as_ref(),
        &caller,
        CreateTodoArgs {
            pool_id: pool_id.clone(),
            title: "ship the feature".into(),
            description: "do it".into(),
            priority: "".into(),
            timeout_secs: 0,
        },
    )
    .await
    .expect("create_todo");

    let err = services::set_pool_status(&store, sink.as_ref(), &caller, &pool_id, "archived")
        .await
        .expect_err("archive must bail when active todos remain");
    assert!(
        err.to_string().contains("active todo"),
        "unexpected archive error: {}",
        err
    );
}

#[tokio::test]
async fn koi_can_only_manage_its_own_todos() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;

    // Pisci creates a todo owned by pisci.
    let pisci = pisci_caller("sess-1");
    let created = services::create_todo(
        &store,
        sink.as_ref(),
        &pisci,
        CreateTodoArgs {
            pool_id: pool_id.clone(),
            title: "pisci task".into(),
            description: "".into(),
            priority: "".into(),
            timeout_secs: 0,
        },
    )
    .await
    .expect("create_todo");
    let todo_id = created["todo"]["id"].as_str().unwrap().to_string();

    // A Koi tries to cancel it — permission denied.
    let koi = koi_caller("koi-alpha", &pool_id);
    let err = services::cancel_todo(&store, sink.as_ref(), &koi, &todo_id, "no reason")
        .await
        .expect_err("koi must not cancel pisci-owned todo");
    assert!(
        err.to_string().contains("Permission denied"),
        "unexpected ownership error: {}",
        err
    );

    // Pisci can cancel it just fine.
    services::cancel_todo(&store, sink.as_ref(), &pisci, &todo_id, "scope changed")
        .await
        .expect("pisci cancel");
    let kinds = sink.drain_kinds();
    assert!(
        kinds.contains(&"todo_changed"),
        "cancel must emit TodoChanged in {:?}",
        kinds
    );
}

#[tokio::test]
async fn assign_koi_creates_todo_posts_mention_and_emits_events() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;

    let caller = pisci_caller("sess-1");
    let subagent = Arc::new(StubSubagentRuntime::always_complete("ok"))
        as Arc<dyn pisci_core::host::SubagentRuntime>;
    let cfg = CoordinatorConfig::default();
    let value = services::assign_koi(
        &store,
        sink_arc(&sink),
        Some(subagent),
        &cfg,
        &caller,
        AssignKoiArgs {
            pool_id: pool_id.clone(),
            koi_id: "koi-alpha".into(),
            task: "build the thing".into(),
            priority: "high".into(),
            timeout_secs: 30,
        },
    )
    .await
    .expect("assign_koi");
    assert_eq!(value["koi_id"], "koi-alpha");

    let kinds = sink.drain_kinds();
    // Assign path emits: todo_changed (created), message_appended
    // (mention), koi_assigned. (The coordinator also spawns a
    // fire-and-forget turn but its events may land later in a separate
    // task, so we only assert the synchronous tail here.)
    assert!(
        kinds
            .windows(3)
            .any(|w| w == ["todo_changed", "message_appended", "koi_assigned"]),
        "expected todo_changed/message_appended/koi_assigned in {:?}",
        kinds
    );
    // Silence the clippy lint about unused modules.
    let _ = pool::session_source::PISCI_POOL;
}

#[tokio::test]
async fn plain_mention_wakes_idle_koi_without_creating_todo() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;
    let caller = pisci_caller("sess-1");
    let requests: Arc<StdMutex<Vec<KoiTurnRequest>>> = Arc::new(StdMutex::new(Vec::new()));
    let requests_cl = requests.clone();
    let subagent = Arc::new(StubSubagentRuntime::new(move |request| {
        requests_cl.lock().unwrap().push(request.clone());
        StubOutcome::Completed("noticed".into())
    })) as Arc<dyn pisci_core::host::SubagentRuntime>;
    let cfg = CoordinatorConfig::default();

    services::send_pool_message(
        &store,
        sink_arc(&sink),
        Some(subagent),
        &cfg,
        &caller,
        SendPoolMessageArgs {
            pool_id: pool_id.clone(),
            sender_id: "pisci".into(),
            content:
                "@Alpha please review the latest status and decide whether follow-up is needed."
                    .into(),
            reply_to_message_id: None,
        },
    )
    .await
    .expect("send_pool_message");

    tokio::time::sleep(Duration::from_millis(25)).await;

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "expected one notification wake-up");
    assert_eq!(requests[0].koi_id, "koi-alpha");
    assert!(
        requests[0].todo_id.is_none(),
        "plain @mention must not pre-create a todo"
    );
    drop(requests);

    let todos = store
        .read(|db| db.list_koi_todos(Some("koi-alpha")))
        .await
        .expect("list todos");
    assert!(
        todos.is_empty(),
        "plain @mention should not create board todos"
    );
}

#[tokio::test]
async fn forced_mention_creates_todo_and_dispatches_execution() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;
    let caller = pisci_caller("sess-1");
    let requests: Arc<StdMutex<Vec<KoiTurnRequest>>> = Arc::new(StdMutex::new(Vec::new()));
    let requests_cl = requests.clone();
    let subagent = Arc::new(StubSubagentRuntime::new(move |request| {
        requests_cl.lock().unwrap().push(request.clone());
        StubOutcome::Completed("done".into())
    })) as Arc<dyn pisci_core::host::SubagentRuntime>;
    let cfg = CoordinatorConfig::default();

    services::send_pool_message(
        &store,
        sink_arc(&sink),
        Some(subagent),
        &cfg,
        &caller,
        SendPoolMessageArgs {
            pool_id: pool_id.clone(),
            sender_id: "pisci".into(),
            content: "@!Alpha implement the migration and report back.".into(),
            reply_to_message_id: None,
        },
    )
    .await
    .expect("send_pool_message");

    tokio::time::sleep(Duration::from_millis(25)).await;

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "expected one delegated wake-up");
    assert_eq!(requests[0].koi_id, "koi-alpha");
    assert!(
        requests[0].todo_id.is_some(),
        "forced @!mention must dispatch against a todo"
    );
    drop(requests);

    let todos = store
        .read(|db| db.list_koi_todos(Some("koi-alpha")))
        .await
        .expect("list todos");
    assert_eq!(todos.len(), 1, "forced @!mention should create a todo");
}

#[tokio::test]
async fn delete_todo_can_batch_delete_cancelled_items_in_pool() {
    let store = build_store();
    let sink = make_sink();
    let pool_id = create_test_pool(&store, &sink).await;
    let pisci = pisci_caller("sess-1");

    let first = services::create_todo(
        &store,
        sink.as_ref(),
        &pisci,
        CreateTodoArgs {
            pool_id: pool_id.clone(),
            title: "cancel me".into(),
            description: "".into(),
            priority: "".into(),
            timeout_secs: 0,
        },
    )
    .await
    .expect("create first");
    let first_id = first["todo"]["id"].as_str().unwrap().to_string();
    services::cancel_todo(&store, sink.as_ref(), &pisci, &first_id, "done elsewhere")
        .await
        .expect("cancel first");

    services::create_todo(
        &store,
        sink.as_ref(),
        &pisci,
        CreateTodoArgs {
            pool_id: pool_id.clone(),
            title: "keep me".into(),
            description: "".into(),
            priority: "".into(),
            timeout_secs: 0,
        },
    )
    .await
    .expect("create second");

    let value = services::delete_todo(
        &store,
        sink.as_ref(),
        &pisci,
        DeleteTodoArgs {
            todo_id: None,
            pool_id: Some(pool_id.clone()),
            status: Some("cancelled".into()),
            owner_id: None,
        },
    )
    .await
    .expect("delete_todo");
    assert_eq!(value["deleted_count"], 1);

    let todos = store
        .read(|db| {
            let all = db.list_koi_todos(None)?;
            Ok::<_, anyhow::Error>(
                all.into_iter()
                    .filter(|todo| todo.pool_session_id.as_deref() == Some(pool_id.as_str()))
                    .collect::<Vec<_>>(),
            )
        })
        .await
        .expect("remaining todos");
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "keep me");
}
