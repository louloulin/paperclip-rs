//! RPC 面：`daemon:rpc_request` → 三条通道级失败 → 独立 task 执行 → `daemon:rpc_response`。
//!
//! 上游对应 `daemonws.Hub.handleRPCFrame`（`hub.go:996`）与 `sendRPCResponse`（`hub.go:1036`）。
//! 判定顺序（解析失败/缺 id → 503 → 429 → 404）是**契约**，本文件逐条验证。

mod hub_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hub_support::*;
use mc_daemon_proto::messages::RPCResponsePayload;
use mc_daemon_proto::rpc;
use mc_ws::frames::{RpcHandler, RpcReply, RpcRequest};
use mc_ws::hub::Hub;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};

fn rpc_request(request_id: &str, method: &str, body: Option<&Value>, timeout_ms: i64) -> String {
    json!({
        "type": "daemon:rpc_request",
        "payload": {
            "request_id": request_id,
            "method": method,
            "body": body,
            "timeout_ms": timeout_ms,
        }
    })
    .to_string()
}

fn response(text: &str) -> RPCResponsePayload {
    let msg = mc_ws::frames::decode(text).expect("response frame 是合法 JSON");
    assert_eq!(msg.kind, mc_daemon_proto::events::DAEMON_RPC_RESPONSE);
    msg.decode_payload().expect("rpc response 载荷")
}

fn recording_handler(
    seen: Arc<Mutex<Vec<RpcRequest>>>,
    reply: impl Fn(&RpcRequest) -> RpcReply + Send + Sync + 'static,
) -> RpcHandler {
    Arc::new(move |request: RpcRequest| {
        let seen = Arc::clone(&seen);
        let reply = reply(&request);
        Box::pin(async move {
            seen.lock().expect("seen lock").push(request);
            reply
        })
    })
}

async fn connected_daemon(hub: Hub) -> (TestServer, Ws) {
    let server = TestServer::start(hub).await;
    let client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;
    (server, client)
}

#[tokio::test]
async fn rpc_request_reaches_the_handler_and_status_passes_through() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::new();
    hub.set_rpc_handler(recording_handler(Arc::clone(&seen), |_| {
        RpcReply::with_status(201, Some(json!({ "task_id": "t-1" })))
    }));
    let (_server, mut client) = connected_daemon(hub).await;

    send_text(
        &mut client,
        &rpc_request(
            "req-1",
            "tasks.claim",
            Some(&json!({ "max_tasks": 4 })),
            1500,
        ),
    )
    .await;

    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-1");
    assert_eq!(payload.status, 201);
    assert_eq!(payload.body, Some(json!({ "task_id": "t-1" })));
    assert!(payload.error.is_empty());

    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "tasks.claim");
    assert_eq!(seen[0].body, Some(json!({ "max_tasks": 4 })));
    assert_eq!(seen[0].timeout_ms, 1500);
    assert_eq!(seen[0].identity.daemon_id, "d-1");
    assert_eq!(seen[0].identity.runtime_ids, vec!["rt-1"]);
}

#[tokio::test]
async fn rpc_unknown_method_is_404_and_never_reaches_the_handler() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::new();
    hub.set_rpc_handler(recording_handler(Arc::clone(&seen), |_| {
        RpcReply::ok(json!({ "never": true }))
    }));
    let (_server, mut client) = connected_daemon(hub).await;

    send_text(&mut client, &rpc_request("req-404", "tasks.nope", None, 0)).await;
    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-404");
    assert_eq!(payload.status, rpc::RPC_STATUS_UNKNOWN_METHOD);
    assert_eq!(payload.error, r#"unknown rpc method "tasks.nope""#);
    assert_eq!(payload.body, None);
    assert!(seen.lock().expect("seen lock").is_empty());
}

#[tokio::test]
async fn rpc_without_a_handler_is_503() {
    let (_server, mut client) = connected_daemon(Hub::new()).await;
    send_text(&mut client, &rpc_request("req-503", "tasks.claim", None, 0)).await;

    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-503");
    assert_eq!(payload.status, rpc::RPC_STATUS_HANDLER_UNAVAILABLE);
    assert_eq!(payload.error, "rpc handler unavailable");
}

#[tokio::test]
async fn rpc_in_flight_is_bounded_and_unknown_method_loses_to_429() {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let hub = Hub::new();
    hub.set_rpc_handler({
        let calls = Arc::clone(&calls);
        let gate = Arc::clone(&gate);
        Arc::new(move |request: RpcRequest| {
            let calls = Arc::clone(&calls);
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let _permit = gate.acquire().await.expect("gate 只在测试结束前关闭");
                RpcReply::ok(json!({ "method": request.method }))
            })
        })
    });
    let (_server, mut client) = connected_daemon(hub).await;

    for index in 1..=rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT {
        send_text(
            &mut client,
            &rpc_request(&format!("req-{index}"), "tasks.claim", None, 0),
        )
        .await;
    }
    wait_until("8 个 handler 都拿到名额", || {
        calls.load(Ordering::SeqCst) == rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT
    })
    .await;

    // 第 9 个（**未知 method**）：名额判定在 method 判定之前 → 429，不是 404。
    send_text(&mut client, &rpc_request("req-9", "tasks.nope", None, 0)).await;
    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-9");
    assert_eq!(payload.status, rpc::RPC_STATUS_TOO_MANY_REQUESTS);
    assert_eq!(payload.error, "too many in-flight rpc requests");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT
    );

    // 8 个在飞的全部还卡着（不排队、也没有被丢弃）。
    assert!(quiet_for(&mut client, QUIET).await);

    gate.add_permits(rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT);
    let mut ids = Vec::new();
    for _ in 0..rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT {
        let payload = response(&expect_text(&mut client, WAIT).await);
        assert_eq!(payload.status, 200);
        assert_eq!(payload.body, Some(json!({ "method": "tasks.claim" })));
        ids.push(payload.request_id);
    }
    ids.sort();
    assert_eq!(
        ids,
        (1..=rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT)
            .map(|index| format!("req-{index}"))
            .collect::<Vec<_>>()
    );

    // 名额释放后，同一个未知 method 才回 404。
    send_text(&mut client, &rpc_request("req-10", "tasks.nope", None, 0)).await;
    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-10");
    assert_eq!(payload.status, rpc::RPC_STATUS_UNKNOWN_METHOD);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT
    );
}

#[tokio::test]
async fn rpc_timeout_budget_is_enforced_and_frees_the_slot() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::new();
    hub.set_rpc_handler({
        let seen = Arc::clone(&seen);
        Arc::new(move |request: RpcRequest| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                seen.lock().expect("seen lock").push(request.clone());
                if request.timeout_ms > 0 {
                    // 远大于调用方给的预算：必定被 transport 掐掉。
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                RpcReply::ok(json!({ "slow": request.timeout_ms > 0 }))
            })
        })
    });
    let (_server, mut client) = connected_daemon(hub).await;

    let started = Instant::now();
    send_text(
        &mut client,
        &rpc_request("req-slow", "tasks.claim", None, 150),
    )
    .await;
    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-slow");
    assert_eq!(payload.status, rpc::RPC_STATUS_INTERNAL);
    assert_eq!(payload.error, "rpc request timed out");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "预算 150ms 的调用等了 {:?}",
        started.elapsed()
    );

    // 名额已释放：`timeout_ms = 0` 的调用（无服务端预算）正常走完。
    send_text(
        &mut client,
        &rpc_request("req-fast", "tasks.claim", None, 0),
    )
    .await;
    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-fast");
    assert_eq!(payload.status, 200);
    assert_eq!(payload.body, Some(json!({ "slow": false })));

    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].timeout_ms, 150);
    assert_eq!(seen[1].timeout_ms, 0);
}

#[tokio::test]
async fn rpc_with_missing_request_id_or_broken_payload_is_dropped() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::new();
    hub.set_rpc_handler(recording_handler(Arc::clone(&seen), |_| {
        RpcReply::ok(json!({ "ok": true }))
    }));
    let (_server, mut client) = connected_daemon(hub).await;

    // 缺 request_id、载荷类型错（timeout_ms 是字符串）、完全不是 JSON 对象。
    send_text(&mut client, &rpc_request("", "tasks.claim", None, 0)).await;
    send_text(
        &mut client,
        r#"{"type":"daemon:rpc_request","payload":{"request_id":"req-x","method":"tasks.claim","timeout_ms":"soon"}}"#,
    )
    .await;
    send_text(
        &mut client,
        r#"{"type":"daemon:rpc_request","payload":"nope"}"#,
    )
    .await;
    assert!(quiet_for(&mut client, QUIET).await, "三条废帧都不该回响应");

    // 屏障帧：合法请求的响应到了，说明前面三条已处理完（读泵顺序执行）。
    send_text(&mut client, &rpc_request("req-ok", "tasks.claim", None, 0)).await;
    let payload = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(payload.request_id, "req-ok");
    assert_eq!(payload.status, 200);
    assert_eq!(seen.lock().expect("seen lock").len(), 1);
}

#[tokio::test]
async fn rpc_handler_observes_cancellation_when_the_connection_goes_away() {
    let entered = Arc::new(AtomicUsize::new(0));
    let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel::<()>();
    let hub = Hub::new();
    hub.set_rpc_handler({
        let entered = Arc::clone(&entered);
        Arc::new(move |request: RpcRequest| {
            let entered = Arc::clone(&entered);
            let cancel = request.cancel.clone();
            let cancel_tx = cancel_tx.clone();
            Box::pin(async move {
                entered.fetch_add(1, Ordering::SeqCst);
                cancel.cancelled().await;
                let _ = cancel_tx.send(());
                RpcReply::failed(499, "connection closed")
            })
        })
    });
    let (server, mut client) = connected_daemon(hub).await;

    send_text(
        &mut client,
        &rpc_request("req-hang", "tasks.claim", None, 0),
    )
    .await;
    wait_until("handler 已进入", || entered.load(Ordering::SeqCst) == 1).await;

    // 客户端掉线 → 读泵退出 → 拆线信号 → handler 从 await 点醒来。
    drop(client);
    tokio::time::timeout(WAIT, cancel_rx.recv())
        .await
        .expect("拆线后 handler 没被取消")
        .expect("取消通道关闭");
    wait_until("连接已注销", || server.hub.connection_count() == 0).await;
}

#[tokio::test]
async fn rpc_response_frames_keep_their_request_id_across_concurrent_calls() {
    let hub = Hub::new();
    hub.set_rpc_handler({
        Arc::new(move |request: RpcRequest| {
            Box::pin(async move {
                // `RpcRequest` 里**没有** request_id（上游 handler 也拿不到，关联由 transport
                // 负责），所以用 body 里的标记来制造「完成顺序与下发顺序相反」。
                let marker = request
                    .body
                    .as_ref()
                    .and_then(|body| body.get("marker"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if marker == "slow" {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                RpcReply::ok(json!({ "echo": marker }))
            })
        })
    });
    let (_server, mut client) = connected_daemon(hub).await;

    send_text(
        &mut client,
        &rpc_request(
            "req-slow",
            "tasks.claim",
            Some(&json!({ "marker": "slow" })),
            0,
        ),
    )
    .await;
    send_text(
        &mut client,
        &rpc_request(
            "req-fast",
            "tasks.claim",
            Some(&json!({ "marker": "fast" })),
            0,
        ),
    )
    .await;

    let first = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(first.request_id, "req-fast");
    assert_eq!(first.body, Some(json!({ "echo": "fast" })));
    let second = response(&expect_text(&mut client, WAIT).await);
    assert_eq!(second.request_id, "req-slow");
    assert_eq!(second.body, Some(json!({ "echo": "slow" })));
}
