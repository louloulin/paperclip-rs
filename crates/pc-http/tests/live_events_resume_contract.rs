//! Live-events WebSocket 重连 resume 契约测试。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use futures_util::{SinkExt, StreamExt};
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
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state_with_realtime(db: Db, realtime: RealtimeHandle) -> AppState {
    let actors = ActorRegistry::new();
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

/// 启动一个监听 127.0.0.1:0 的 axum server，返回 base URL。
async fn spawn_app(state: AppState) -> String {
    use axum::Router;
    let app: Router = Router::new()
        .merge(routes::live_events::router())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}")
}

async fn seed_company(db: &Db) -> Uuid {
    let prefix = format!("LE{}", &Uuid::new_v4().simple().to_string()[..4]);
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(format!("live-events-test-{}", Uuid::new_v4().simple()))
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("seed company")
}

async fn seed_agent_api_key(db: &Db, company_id: Uuid) -> (String, Uuid) {
    // Insert an agent + agent_api_keys row. Mirrors the Node live-events-ws
    // authorization path which queries `agentApiKeys` keyed on the SHA-256 of
    // the bearer token. `agent_api_keys` is agent-scoped and contains both
    // `agent_id` (FK → agents) and `company_id` (FK → companies).
    use rand::Rng;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill(&mut buf);
    let token = format!("test-tok-{}", Uuid::new_v4().simple());
    let key_hash = pc_auth::hash_token(&token);

    let agent_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agents (company_id, name, adapter_type) \
         VALUES ($1, $2, 'process') RETURNING id",
    )
    .bind(company_id)
    .bind(format!("live-events-agent-{}", Uuid::new_v4().simple()))
    .fetch_one(db.pool())
    .await
    .expect("seed agent");

    let key_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_api_keys (agent_id, company_id, name, key_hash) \
         VALUES ($1, $2, 'test', $3) RETURNING id",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(&key_hash)
    .fetch_one(db.pool())
    .await
    .expect("seed key");
    (token, key_id)
}

#[tokio::test(flavor = "current_thread")]
async fn live_events_resume_replays_missed_events() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = seed_company(&db).await;
    let (token, _kid) = seed_agent_api_key(&db, company_id).await;

    let realtime = RealtimeHandle::start_with_replay(64, 64);
    // 先发布 3 个事件，再让客户端连接 resume 从 0 开始 → 应收到全部 3 个 replay + welcome
    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let id3 = Uuid::new_v4();
    realtime.publish(pc_realtime::LiveEvent::new("test.first", "x", id1).with_company(company_id));
    realtime.publish(pc_realtime::LiveEvent::new("test.second", "x", id2).with_company(company_id));
    realtime.publish(pc_realtime::LiveEvent::new("test.third", "x", id3).with_company(company_id));

    let state = test_state_with_realtime(db, realtime.clone());
    let url = spawn_app(state).await;
    let ws_url = format!(
        "{}/api/live-events?token={}&company_id={}&resume=0",
        url, token, company_id
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("ws connect");
    // 期望收到：3 条 replay 事件 + 1 条 resumed ack + 1 条 welcome
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while events.len() < 5 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                events.push(v);
            }
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(_)))) => continue,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break,
        }
    }

    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().unwrap_or(""))
        .collect();
    assert!(
        kinds.contains(&"event"),
        "expected 'event' frame: {events:?}"
    );
    assert!(
        kinds.contains(&"resumed"),
        "expected 'resumed' frame: {events:?}"
    );
    assert!(
        kinds.contains(&"welcome"),
        "expected 'welcome' frame: {events:?}"
    );
    let replayed_count = events
        .iter()
        .find(|e| e["type"] == "resumed")
        .and_then(|e| e["replayed"].as_u64())
        .unwrap_or(0);
    assert_eq!(replayed_count, 3, "should have replayed 3 events");
    let _ = ws.close(None);
}

#[tokio::test(flavor = "current_thread")]
async fn live_events_no_resume_just_gets_welcome() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = seed_company(&db).await;
    let (token, _kid) = seed_agent_api_key(&db, company_id).await;

    let realtime = RealtimeHandle::start_with_replay(64, 64);
    // 即使有 replay buffer，无 resume 参数也不应重放
    realtime.publish(
        pc_realtime::LiveEvent::new("test.orphan", "x", Uuid::new_v4()).with_company(company_id),
    );

    let state = test_state_with_realtime(db, realtime.clone());
    let url = spawn_app(state).await;
    let ws_url = format!(
        "{}/api/live-events?token={}&company_id={}",
        url, token, company_id
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("ws connect");
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while events.len() < 3 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                events.push(v);
            }
            _ => break,
        }
    }
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().unwrap_or(""))
        .collect();
    assert!(kinds.contains(&"welcome"));
    // 没有 resumed 帧（无 resume 参数）
    assert!(
        !kinds.contains(&"resumed"),
        "should not emit resumed without resume param"
    );
    let _ = ws.close(None);
}

#[tokio::test(flavor = "current_thread")]
async fn live_events_resume_from_high_id_skips_misses() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = seed_company(&db).await;
    let (token, _kid) = seed_agent_api_key(&db, company_id).await;

    let realtime = RealtimeHandle::start_with_replay(64, 64);
    realtime
        .publish(pc_realtime::LiveEvent::new("e1", "x", Uuid::new_v4()).with_company(company_id));
    realtime
        .publish(pc_realtime::LiveEvent::new("e2", "x", Uuid::new_v4()).with_company(company_id));
    realtime
        .publish(pc_realtime::LiveEvent::new("e3", "x", Uuid::new_v4()).with_company(company_id));
    // 用第 2 个 event_id 作为 resume 起点 → 应只收到第 3 条
    let all = realtime.replay_after(0);
    assert_eq!(all.len(), 3);
    let resume_from = all[1].event_id;

    let state = test_state_with_realtime(db, realtime.clone());
    let url = spawn_app(state).await;
    let ws_url = format!(
        "{}/api/live-events?token={}&company_id={}&resume={}",
        url, token, company_id, resume_from
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("ws connect");
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while events.len() < 3 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                events.push(v);
            }
            _ => break,
        }
    }
    // 只应收到 1 条 event + 1 条 resumed (replayed=1) + welcome
    let replayed = events
        .iter()
        .find(|e| e["type"] == "resumed")
        .and_then(|e| e["replayed"].as_u64())
        .unwrap_or(0);
    assert_eq!(replayed, 1, "expected 1 replay, got events={events:?}");
    let _ = ws.close(None);
}

#[tokio::test(flavor = "current_thread")]
async fn live_events_invalid_token_rejects_with_401() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let _company_id = seed_company(&db).await;

    let realtime = RealtimeHandle::start_with_replay(64, 64);
    let state = test_state_with_realtime(db, realtime);
    let url = spawn_app(state).await;
    let ws_url = format!("{}/api/live-events?token=invalid-tok", url);

    let resp = tokio_tungstenite::connect_async(ws_url).await;
    // 401 → handshake failure
    assert!(resp.is_err(), "ws with invalid token should fail handshake");
}
