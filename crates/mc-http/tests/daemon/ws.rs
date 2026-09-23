//! `GET /api/daemon/ws` 的真 socket 用例（握手 + 帧收发 + RPC 回包）。
//!
//! 只能起真监听：`tower::ServiceExt::oneshot` 拿到的是**未升级**的响应，读泵不会跑。
//! 帧格式取自冻结协议（`{"type": …, "payload": …}`），与 daemon 真正发的字节一致。

use axum::http::{HeaderValue, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use crate::support::{self, USER_ID_HEADER};

/// 建一条已升级的连接，断言 101。
async fn connect(
    base: &str,
    user_id: Uuid,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut request = format!("{base}/api/daemon/ws")
        .into_client_request()
        .expect("client request");
    request.headers_mut().insert(
        USER_ID_HEADER,
        HeaderValue::from_str(&user_id.to_string()).expect("header value"),
    );
    let (socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("ws handshake");
    assert_eq!(
        response.status(),
        StatusCode::SWITCHING_PROTOCOLS,
        "daemon ws 必须升级"
    );
    socket
}

/// 发一帧（客户端永远是 Text + JSON）。
async fn send(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    kind: &str,
    payload: Value,
) {
    let frame = json!({ "type": kind, "payload": payload }).to_string();
    socket
        .send(WsMessage::Text(frame.into()))
        .await
        .expect("send frame");
}

/// 收一帧并解析成 `{type, payload}`（超时即判失败，避免测试挂死）。
async fn recv(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
        .expect("ws frame within 10s")
        .expect("stream open")
        .expect("frame ok");
    let text = frame.into_text().expect("text frame");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("frame json ({e}): {text}"))
}

/// 握手 + 未知 method 的 RPC 回包（404）+ 心跳越权不发 ack + `tasks.claim` 与 HTTP 腿同体。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn ws_handshake_rpc_and_claim_share_the_http_body() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db.clone());
    let (runtime_id, agent_id, task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;
    let _ = agent_id;
    let (base, server) = support::spawn_server(app.clone()).await;

    let mut socket = connect(&base, user_id).await;

    // 用户身份连接的 scope 里没有 runtime（`runtime_ids` 只由 `mdt_` token 填），所以这条
    // 心跳**不该**收到 ack。顺序栅栏：紧接着发一条未知 method 的 RPC，收到的第一帧必须是
    // 它的回包 —— 否则就说明心跳 ack 抢在前面了。
    send(
        &mut socket,
        "daemon:heartbeat",
        json!({ "runtime_id": runtime_id.to_string(), "supports_batch_import": true }),
    )
    .await;
    send(
        &mut socket,
        "daemon:rpc_request",
        json!({ "request_id": "r-unknown", "method": "tasks.nope" }),
    )
    .await;
    let reply = recv(&mut socket).await;
    assert_eq!(reply["type"], json!("daemon:rpc_response"), "{reply}");
    assert_eq!(reply["payload"]["request_id"], json!("r-unknown"));
    assert_eq!(
        reply["payload"]["status"],
        json!(404),
        "未知 method 是 404（不是 500）: {reply}"
    );
    assert!(
        reply["payload"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("unknown rpc method")),
        "{reply}"
    );

    // `tasks.claim` 与 HTTP 腿**逐字同体**：同一个 `claim_batch_core`，两条路都回
    // `{"tasks":[…]}`（max_tasks=0 的短路让两边的字节可逐字比较）。
    send(
        &mut socket,
        "daemon:rpc_request",
        json!({
            "request_id": "r-claim-empty",
            "method": "tasks.claim",
            "body": { "daemon_id": "m1", "runtime_ids": [runtime_id.to_string()], "max_tasks": 0 },
        }),
    )
    .await;
    let reply = recv(&mut socket).await;
    assert_eq!(reply["payload"]["status"], json!(200), "{reply}");
    let ws_body = reply["payload"]["body"].clone();
    assert_eq!(ws_body, json!({ "tasks": [] }), "{ws_body}");

    let (http_status, http_body) = support::call(
        &app,
        "POST",
        "/api/daemon/tasks/claim",
        user_id,
        Some("m1"),
        Some(json!({
            "daemon_id": "m1",
            "runtime_ids": [runtime_id.to_string()],
            "max_tasks": 0,
        })),
    )
    .await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(ws_body, http_body, "ws 与 http 的响应该逐字相同");

    // 真正领一条：WS 腿也能拿到任务（证明它走的是同一段实现，不是只读的空分支）。
    send(
        &mut socket,
        "daemon:rpc_request",
        json!({
            "request_id": "r-claim-one",
            "method": "tasks.claim",
            "body": { "daemon_id": "m1", "runtime_ids": [runtime_id.to_string()], "max_tasks": 4 },
        }),
    )
    .await;
    let reply = recv(&mut socket).await;
    assert_eq!(reply["payload"]["status"], json!(200), "{reply}");
    assert_eq!(
        reply["payload"]["body"]["tasks"][0]["id"],
        json!(task_id.to_string()),
        "{reply}"
    );

    // 重连：拆线后再建一条新连接，仍然 101（无连接租约可残留）。
    socket.close(None).await.expect("close");
    drop(socket);
    let mut again = connect(&base, user_id).await;
    again.close(None).await.expect("close again");

    server.abort();
    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 无任何身份（既无 token 也无用户头）⇒ 400 且**不升级**（上游 `identity.validate()`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn ws_without_identity_is_rejected_before_upgrade() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (base, server) = support::spawn_server(app).await;

    let request = format!("{base}/api/daemon/ws")
        .into_client_request()
        .expect("client request");
    let error = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("无身份不该升级");
    let text = error.to_string();
    assert!(
        text.contains("400") || text.contains("Bad Request"),
        "期望 400：{text}"
    );

    server.abort();
    support::cleanup(&pool, workspace_id, &[user_id]).await;
}
