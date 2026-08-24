//! Board chat thread/message 持久化契约测试。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState::new(realtime.clone(), "test".to_string())),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("request"))
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

async fn seed_company(db: &Db) -> Uuid {
    let prefix = Uuid::new_v4().simple().to_string();
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(format!("board-chat-test-{}", Uuid::new_v4().simple()))
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("seed company")
}

#[tokio::test(flavor = "current_thread")]
async fn list_threads_is_empty_for_fresh_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::board_chat::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/board-chat/threads"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "list threads: {body}");
    assert_eq!(body["companyId"], json!(company_id.to_string()));
    assert!(body["items"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn list_messages_for_unknown_thread_returns_empty() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::board_chat::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/board/chat/threads/{}/messages", Uuid::new_v4()),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "list messages: {body}");
    assert!(body["items"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn board_chat_repo_appends_messages_round_trip() {
    use pc_repos::board_chat::{BoardChatRepo, ChatMessageStatus, ChatRole, NewMessage, NewThread};
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = seed_company(&db).await;
    let repo = BoardChatRepo::new(&db);

    let thread = repo
        .get_or_create_thread(&NewThread {
            company_id,
            issue_id: None,
            title: "Round Trip".into(),
            created_by_user_id: None,
        })
        .await
        .expect("create thread");
    assert_eq!(thread.company_id, company_id);
    assert_eq!(thread.title, "Round Trip");

    let _ = repo
        .append_message(&NewMessage {
            thread_id: thread.id,
            company_id,
            role: ChatRole::User,
            author_user_id: None,
            author_agent_id: None,
            body: "hello".into(),
            tool_uses: None,
            status: Some(ChatMessageStatus::Complete),
        })
        .await
        .expect("append user");
    let _ = repo
        .append_message(&NewMessage {
            thread_id: thread.id,
            company_id,
            role: ChatRole::Assistant,
            author_user_id: None,
            author_agent_id: None,
            body: "world".into(),
            tool_uses: None,
            status: Some(ChatMessageStatus::Complete),
        })
        .await
        .expect("append assistant");

    let messages = repo.list_messages(thread.id, 10).await.expect("list");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].body, "hello");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].body, "world");
}

#[tokio::test(flavor = "current_thread")]
async fn thread_gets_both_user_and_assistant_messages() {
    use pc_repos::board_chat::{BoardChatRepo, ChatRole, NewMessage, NewThread};
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = seed_company(&db).await;
    let repo = BoardChatRepo::new(&db);
    let thread = repo
        .get_or_create_thread(&NewThread {
            company_id,
            issue_id: None,
            title: "Both Roles".into(),
            created_by_user_id: None,
        })
        .await
        .expect("create thread");
    // user turn
    repo.append_message(&NewMessage {
        thread_id: thread.id,
        company_id,
        role: ChatRole::User,
        author_user_id: None,
        author_agent_id: None,
        body: "first question".into(),
        tool_uses: None,
        status: None,
    })
    .await
    .expect("user msg");
    // assistant turn
    repo.append_message(&NewMessage {
        thread_id: thread.id,
        company_id,
        role: ChatRole::Assistant,
        author_user_id: None,
        author_agent_id: None,
        body: "first answer".into(),
        tool_uses: None,
        status: None,
    })
    .await
    .expect("assistant msg");
    // a second turn to verify ordering
    repo.append_message(&NewMessage {
        thread_id: thread.id,
        company_id,
        role: ChatRole::User,
        author_user_id: None,
        author_agent_id: None,
        body: "followup".into(),
        tool_uses: None,
        status: None,
    })
    .await
    .expect("user 2");
    repo.append_message(&NewMessage {
        thread_id: thread.id,
        company_id,
        role: ChatRole::Assistant,
        author_user_id: None,
        author_agent_id: None,
        body: "answer 2".into(),
        tool_uses: None,
        status: None,
    })
    .await
    .expect("assistant 2");
    let messages = repo.list_messages(thread.id, 100).await.expect("list");
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[2].role, "user");
    assert_eq!(messages[3].role, "assistant");
    assert_eq!(messages[0].body, "first question");
    assert_eq!(messages[3].body, "answer 2");
}
