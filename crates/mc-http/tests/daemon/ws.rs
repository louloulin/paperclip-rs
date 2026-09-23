//! `GET /api/daemon/ws` 的真 socket 用例（握手 + 帧收发 + RPC 回包）。
//!
//! 只能起真监听：`tower::ServiceExt::oneshot` 拿到的是**未升级**的响应，读泵不会跑。
//! 帧格式取自冻结协议（`{"type": …, "payload": …}`），与 daemon 真正发的字节一致。

use axum::http::{HeaderValue, StatusCode};
use futures_util::{SinkExt, StreamExt};
use mc_ws::hub::DeliveryOutcome;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use crate::support::{self, USER_ID_HEADER};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 建一条已升级的连接，断言 101（`extra` = 附加请求头，例：`Authorization: Bearer mdt_…`）。
async fn connect_url(url: &str, extra: &[(&str, &str)]) -> Socket {
    let mut request = url.into_client_request().expect("client request");
    for (name, value) in extra {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
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

/// 建一条已升级的连接，断言 101。
async fn connect(base: &str, user_id: Uuid) -> Socket {
    connect_url(
        &format!("{base}/api/daemon/ws"),
        &[(USER_ID_HEADER, &user_id.to_string())],
    )
    .await
}

/// 发一帧（客户端永远是 Text + JSON）。
async fn send(socket: &mut Socket, kind: &str, payload: Value) {
    let frame = json!({ "type": kind, "payload": payload }).to_string();
    socket
        .send(WsMessage::Text(frame))
        .await
        .expect("send frame");
}

/// 收一帧并解析成 `{type, payload}`（超时即判失败，避免测试挂死）。
async fn recv(socket: &mut Socket) -> Value {
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

/// 握手被 404 拒掉，且错误正文是**上游原文** `runtime not found`。
///
/// tungstenite 只把状态码放进 `Display`，错误正文在 `Error::Http` 的响应体里 —— 所以必须
/// 拆开 variant 读 body，不能拿 `to_string()` 找字串。
fn assert_handshake_404(error: tokio_tungstenite::tungstenite::Error, what: &str) {
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("{what}：期望 HTTP 404 拒绝，实为 {error}");
    };
    assert_eq!(response.status(), StatusCode::NOT_FOUND, "{what}");
    let body = String::from_utf8(response.body().clone().unwrap_or_default()).expect("utf8 body");
    assert!(
        body.contains("runtime not found"),
        "{what}：期望上游原文 `runtime not found`，实为 {body}"
    );
}

/// 轮询到条件成立（连接注册是异步的）或超时。
async fn wait_until(label: &str, mut probe: impl FnMut() -> bool) {
    for _ in 0..200 {
        if probe() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("等待超时：{label}");
}

/// `?runtime_id=` / `?runtime_ids=` 收窄投递面（上游 `daemon_ws.go` `parseRuntimeIDs` +
/// `buildDaemonWebSocketIdentity`）：
///
/// * 只带 `runtime_id=A` 的连接**收得到**发往 A 的 `daemon:task_available`；
/// * 同一条连接**收不到**发往 B 的 —— 收窄后的 `ClientIdentity.runtime_ids` 决定 hub 的
///   `by_runtime` 索引，所以这是真行为而不只是参数校验。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn ws_runtime_query_narrows_the_delivery_scope() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    // 同一台机器下两个 runtime：收窄到 a，b 就出了投递面。
    let rt_a = support::seed_runtime(&pool, workspace_id, user_id, "m1").await;
    // 同 `(workspace, daemon)` 下第二台 runtime 得换 provider（唯一索引）。
    let rt_b =
        support::seed_runtime_with_provider(&pool, workspace_id, user_id, "m1", "codex").await;
    let (app, state) = support::app_and_state(db);
    let (base, server) = support::spawn_server(app).await;
    let token = support::seed_daemon_token(&pool, workspace_id, "m1").await;

    let mut socket = connect_url(
        &format!("{base}/api/daemon/ws?runtime_id={rt_a}"),
        &[("authorization", &format!("Bearer {token}"))],
    )
    .await;
    let narrowed_key = rt_a.to_string();
    wait_until("连接注册到收窄后的 runtime", || {
        state.daemon_hub.runtime_connection_count(&narrowed_key) == 1
    })
    .await;
    assert_eq!(
        state.daemon_hub.runtime_connection_count(&rt_b.to_string()),
        0,
        "被收窄掉的 runtime 不该进索引"
    );

    // a 在 scope 内 ⇒ 送达。
    assert_eq!(
        state
            .daemon_hub
            .notify_task_available(&narrowed_key, "task-a"),
        DeliveryOutcome::hit()
    );
    let frame = recv(&mut socket).await;
    assert_eq!(frame["type"], json!("daemon:task_available"), "{frame}");
    assert_eq!(frame["payload"]["runtime_id"], json!(narrowed_key));

    // b 不在 scope 内 ⇒ **miss**（没有连接订阅它），也就没有帧。
    assert_eq!(
        state
            .daemon_hub
            .notify_task_available(&rt_b.to_string(), "task-b"),
        DeliveryOutcome::miss()
    );

    // 逗号形态等价：`?runtime_ids=A,B` 把两个都装回来。
    let token_two = support::seed_daemon_token(&pool, workspace_id, "m1").await;
    let mut both = connect_url(
        &format!("{base}/api/daemon/ws?runtime_ids={rt_a},{rt_b}"),
        &[("authorization", &format!("Bearer {token_two}"))],
    )
    .await;
    let other_key = rt_b.to_string();
    wait_until("两条 runtime 都进索引", || {
        state.daemon_hub.runtime_connection_count(&other_key) == 1
    })
    .await;
    assert_eq!(
        state.daemon_hub.notify_task_available(&other_key, "task-b"),
        DeliveryOutcome::hit()
    );
    let frame = recv(&mut both).await;
    assert_eq!(frame["type"], json!("daemon:task_available"), "{frame}");
    assert_eq!(frame["payload"]["task_id"], json!("task-b"));

    socket.close(None).await.expect("close");
    both.close(None).await.expect("close");
    server.abort();
    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 请求了**不属于该机器**的 runtime ⇒ 握手 404 `runtime not found`（上游同文案同码）。
/// 用户身份连接带收窄参数也 fail-closed 404（本地不加载用户可见 runtime 集，见 `docs/44`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn ws_runtime_query_rejects_a_runtime_that_is_not_owned() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let rt_mine = support::seed_runtime(&pool, workspace_id, user_id, "m1").await;
    // 另**一台**机器的 runtime：对 m1 的 token 来说不在名下。
    let rt_other = support::seed_runtime(&pool, workspace_id, user_id, "m2").await;
    let app = support::app_with_db(db);
    let (base, server) = support::spawn_server(app).await;
    let token = support::seed_daemon_token(&pool, workspace_id, "m1").await;

    for query in [
        format!("runtime_id={rt_other}"),
        format!("runtime_ids={rt_mine},{rt_other}"),
    ] {
        let request = format!("{base}/api/daemon/ws?{query}")
            .into_client_request()
            .expect("client request");
        let mut request = request;
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("header value"),
        );
        let error = tokio_tungstenite::connect_async(request)
            .await
            .expect_err("不归该 daemon 的 runtime 不该升级");
        assert_handshake_404(error, "不归该 daemon 的 runtime");
    }

    // 用户身份连接声明 runtime 同样 404（本片 fail-closed 口径）。
    let request = format!("{base}/api/daemon/ws?runtime_id={rt_mine}")
        .into_client_request()
        .expect("client request");
    let mut request = request;
    request.headers_mut().insert(
        axum::http::HeaderName::from_bytes(USER_ID_HEADER.as_bytes()).expect("header name"),
        HeaderValue::from_str(&user_id.to_string()).expect("header value"),
    );
    let error = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("用户连接的 runtime 收窄本地 fail-closed");
    assert_handshake_404(error, "用户连接声明 runtime");

    server.abort();
    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// **端到端**：`PUT /api/agents/:id/env` → `agent:status` 帧（HTTP 写入 → hub → 用户 socket）。
///
/// 同时锁两个不变量：① 帧里**没有 env 明文**（载荷是脱敏的 `AgentDto`）；
/// ② 同一工作区的 **daemon 面**连接收不到（用户面事件的投递面过滤）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn env_update_broadcasts_agent_status_to_user_connections_only() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let (runtime_id, agent_id, _task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;
    let (app, _state) = support::app_and_state(db);
    let (base, server) = support::spawn_server(app.clone()).await;
    let token = support::seed_daemon_token(&pool, workspace_id, "m1").await;

    let mut user_socket = connect(&base, user_id).await;
    let mut daemon_socket = connect_url(
        &format!("{base}/api/daemon/ws?runtime_id={runtime_id}"),
        &[("authorization", &format!("Bearer {token}"))],
    )
    .await;
    // 顺序栅栏：两条连接都注册好之前不发 PUT（否则可能错过广播）。
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let (status, body) = support::call(
        &app,
        "PUT",
        &format!("/api/agents/{agent_id}/env?workspace_id={workspace_id}"),
        user_id,
        None,
        Some(json!({ "custom_env": { "SECRET_TOKEN": "bar" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 用户连接收到 `agent:status`，载荷里的 agent id 是刚改的那一个。
    let frame = recv(&mut user_socket).await;
    assert_eq!(frame["type"], json!("agent:status"), "{frame}");
    assert_eq!(frame["payload"]["agent"]["id"], json!(agent_id.to_string()));
    // 脱敏：env 值绝不出现在帧里。
    let text = frame.to_string();
    assert!(!text.contains("bar"), "{text}");
    assert!(!text.contains("SECRET_TOKEN"), "{text}");

    // daemon 面连接：顺序栅栏——紧接的未知 method RPC 回包必须是**第一**帧。
    send(
        &mut daemon_socket,
        "daemon:rpc_request",
        json!({ "request_id": "r-after-env", "method": "tasks.nope" }),
    )
    .await;
    let reply = recv(&mut daemon_socket).await;
    assert_eq!(reply["type"], json!("daemon:rpc_response"), "{reply}");
    assert_eq!(reply["payload"]["request_id"], json!("r-after-env"));

    user_socket.close(None).await.expect("close");
    daemon_socket.close(None).await.expect("close");
    server.abort();
    support::cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 无任何身份（既无 token 也无用户头）⇒ **401** 且不升级。
///
/// 判据是上游 `middleware/daemon_auth.go`:102-106：`/api/daemon` 整组挂在 `DaemonAuth`
/// 之下（`router.go:1520-1521`），而 `DaemonAuth` 在**没有 `Authorization` 头**时直接
/// `401 missing authorization header`，中间件先于 handler 结束 —— `DaemonWebSocket` 里
/// 那个 400 根本走不到（它只在“凭据有效但既没 `runtime_ids` 也没用户”时触发，
/// `daemon_ws.go:19-23`）。本仓 `DaemonAuth`（`scope.rs`）同样先 401，行为一致。
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
        text.contains("401") || text.contains("Unauthorized"),
        "期望 401（上游 `DaemonAuth` 在缺 Authorization 头时先于 handler 结束）：{text}"
    );

    server.abort();
    support::cleanup(&pool, workspace_id, &[user_id]).await;
}
