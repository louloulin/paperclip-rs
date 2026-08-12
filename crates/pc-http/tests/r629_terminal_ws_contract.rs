//! R629: terminal-ws WebSocket 集成测试。
//!
//! 真实 axum server（含 environments router）+ tokio-tungstenite
//! 完成 WS upgrade → ready → output → resize/raw → close 全链路验证。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use futures_util::{SinkExt, StreamExt};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::terminal::{
    FakeSshConnector, InMemoryStore, TerminalSessionRecord, TerminalSessionStore,
    TerminalSshConnector,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn build_state(
    db: Db,
    realtime: RealtimeHandle,
    store: Arc<dyn TerminalSessionStore>,
    connector: Arc<dyn TerminalSshConnector>,
) -> AppState {
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
    .with_terminal_runtime(store, connector)
}

async fn spawn_app(state: AppState) -> String {
    let app: Router = Router::new()
        .merge(routes::environments::router())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}")
}

async fn collect_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    deadline: Duration,
) -> Option<Value> {
    match tokio::time::timeout(deadline, ws.next()).await {
        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
            serde_json::from_str(&t).ok()
        }
        _ => None,
    }
}

/// R629 happy path: WS upgrade → ready → output → resize/raw → close。
#[tokio::test(flavor = "current_thread")]
async fn terminal_ws_full_lifecycle() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let realtime = RealtimeHandle::start(64);

    // seed 一个有效 terminal session（先 concrete insert，再 cast 到 trait object）
    let store_concrete = Arc::new(InMemoryStore::new());
    let setup_id = "550e8400-e29b-41d4-a716-446655440000";
    let term_id = "660e8400-e29b-41d4-a716-446655440001";
    store_concrete.insert(TerminalSessionRecord {
        id: term_id.into(),
        setup_session_id: setup_id.into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        ssh_host: "127.0.0.1".into(),
        ssh_port: 22,
        ssh_username: "root".into(),
    });
    let store: Arc<dyn TerminalSessionStore> = store_concrete;

    // fake connector 预录两条输出
    let connector: Arc<dyn TerminalSshConnector> = Arc::new(FakeSshConnector {
        verify_returns: true,
        connect_error: None,
        data_script: vec![
            pc_realtime::terminal::traits::ShellEvent::Data("hello\n".into()),
            pc_realtime::terminal::traits::ShellEvent::Data("$ ".into()),
        ],
    });

    let state = build_state(db, realtime, store, connector);
    let url = spawn_app(state).await;
    let ws_url = format!(
        "{}/api/environment-custom-image-setup-sessions/{}/terminal/ws?terminal_session_id={}&token=test-token",
        url, setup_id, term_id
    );

    let (mut ws, resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("ws upgrade");
    assert_eq!(resp.status().as_u16(), 101, "WS upgrade expected 101");

    // 1. ready 帧
    let ready = collect_frame(&mut ws, Duration::from_secs(2))
        .await
        .expect("ready");
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["setupSessionId"], setup_id);

    // 2/3. 两条 output 帧
    let out1 = collect_frame(&mut ws, Duration::from_secs(2))
        .await
        .expect("output 1");
    assert_eq!(out1["type"], "output");
    assert_eq!(out1["data"], "hello\n");
    let out2 = collect_frame(&mut ws, Duration::from_secs(2))
        .await
        .expect("output 2");
    assert_eq!(out2["type"], "output");
    assert_eq!(out2["data"], "$ ");

    // 4. resize + raw
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type":"resize","cols":120,"rows":40})
            .to_string()
            .into(),
    ))
    .await
    .expect("send resize");
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type":"raw","data":"ls\n"}).to_string().into(),
    ))
    .await
    .expect("send raw");

    // 5. 关闭
    ws.send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await
        .expect("close");
    let _ = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
}

/// R629: 缺 terminal_session_id → 400 JSON。
#[tokio::test(flavor = "current_thread")]
async fn terminal_ws_rejects_missing_query_params() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let realtime = RealtimeHandle::start(64);

    let store_concrete = Arc::new(InMemoryStore::new());
    let store: Arc<dyn TerminalSessionStore> = store_concrete;
    let connector: Arc<dyn TerminalSshConnector> = Arc::new(FakeSshConnector::default());
    let state = build_state(db, realtime, store, connector);

    let url = spawn_app(state).await;
    let ws_url = format!(
        "{}/api/environment-custom-image-setup-sessions/setup-x/terminal/ws",
        url
    );

    // 缺 terminal_session_id → 应返回 400 而非 101 升级
    let result = tokio_tungstenite::connect_async(ws_url).await;
    assert!(
        result.is_err(),
        "missing terminal_session_id should fail WS handshake (got: {:?})",
        result.map(|(_, r)| r.status())
    );
}

/// R629: 未注入 runtime → 503。
#[tokio::test(flavor = "current_thread")]
async fn terminal_ws_returns_503_when_runtime_missing() {
    let _guard = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let realtime = RealtimeHandle::start(64);
    let actors = ActorRegistry::new();
    let state = AppState::new(
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
    );

    let url = spawn_app(state).await;
    let ws_url = format!(
        "{}/api/environment-custom-image-setup-sessions/setup-x/terminal/ws?terminal_session_id=t1&token=tk",
        url
    );
    let result = tokio_tungstenite::connect_async(ws_url).await;
    assert!(
        result.is_err(),
        "503 should reject WS upgrade (got: {:?})",
        result.map(|(_, r)| r.status())
    );
}
