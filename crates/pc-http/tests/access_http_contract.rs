//! `/api/board-api-keys*` 与 `/api/board-claim*` 路由契约测试。

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
        Arc::new(WsState::new(realtime.clone(), "test")),
        realtime,
    )
}

async fn insert_user(db: &Db, user_id: &str) {
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, true, now(), now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(format!("User {user_id}"))
    .bind(format!("{user_id}@example.com"))
    .execute(db.pool())
    .await
    .expect("insert user");
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = format!("sess_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO session (id, expires_at, token, created_at, updated_at, user_id) \
         VALUES ($1, now() + interval '1 hour', $2, now(), now(), $3) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("sess-{user_id}-{}", Uuid::new_v4().simple()))
    .bind(&token)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert session");
    token
}

async fn call_with_session(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    session_token: &str,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {session_token}"))
                .uri(path)
                .body(Body::from(payload))
                .expect("request"),
        )
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

#[tokio::test(flavor = "current_thread")]
async fn board_key_create_persists_real_sha256_hash_and_returns_one_time_token() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    insert_user(&db, "board-user-1").await;
    let session = insert_session(&db, "board-user-1").await;
    let app = routes::access::router().with_state(test_state(db.clone()));

    let (status, body) = call_with_session(
        &app,
        "POST",
        "/api/board-api-keys",
        Some(json!({ "name": "CLI token" })),
        &session,
    )
    .await;
    assert_eq!(status, 201, "board key create: {body}");
    let key_id = body["id"].as_str().expect("id");
    let token = body["token"].as_str().expect("one-time token");
    assert!(token.starts_with("pk_") || token.starts_with("pcp_board_"), "token format: {token}");
    assert_ne!(token, "key-hash-stub");

    // DB-stored hash is SHA-256(token) hex
    let stored: String = sqlx::query_scalar("SELECT key_hash FROM board_api_keys WHERE id=$1")
        .bind(Uuid::parse_str(key_id).unwrap())
        .fetch_one(db.pool())
        .await
        .expect("load key hash");
    use sha2::{Digest, Sha256};
    let expected = {
        let mut h = Sha256::new();
        h.update(token.as_bytes());
        hex::encode(h.finalize())
    };
    assert_eq!(stored, expected);
    assert_eq!(stored.len(), 64);

    // Listing returns same key with no plaintext token
    let (list_status, list_body) =
        call_with_session(&app, "GET", "/api/board-api-keys", None, &session).await;
    assert_eq!(list_status, 200);
    let arr = list_body["items"].as_array().expect("list items is array");
    assert!(arr.iter().any(|k| k["id"] == key_id));
    let listed = arr
        .iter()
        .find(|k| k["id"] == key_id)
        .expect("find created key");
    assert!(
        listed.get("token").is_none(),
        "list must not leak plaintext token"
    );

    // Revoke
    let (del_status, _) = call_with_session(
        &app,
        "DELETE",
        &format!("/api/board-api-keys/{key_id}"),
        None,
        &session,
    )
    .await;
    assert_eq!(del_status, 204);
}
