//! 心跳面：`daemon:heartbeat` → scope 校验 → handler → `daemon:heartbeat_ack`（真 socket）。
//!
//! 上游对应 `daemonws.Hub.handleHeartbeatFrame`（`hub.go:1068`）。

mod hub_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hub_support::*;
use mc_daemon_proto::messages::DaemonHeartbeatAckPayload;
use mc_ws::frames::{HeartbeatHandler, HeartbeatRequest};
use mc_ws::hub::{DeliveryOutcome, Hub};

/// 记录 handler 收到的每次请求的 handler。
fn recording_handler(
    seen: Arc<Mutex<Vec<HeartbeatRequest>>>,
    reply: impl Fn(&HeartbeatRequest) -> Result<Option<DaemonHeartbeatAckPayload>, String>
        + Send
        + Sync
        + 'static,
) -> HeartbeatHandler {
    Arc::new(move |request: HeartbeatRequest| {
        let seen = Arc::clone(&seen);
        let reply = reply(&request);
        Box::pin(async move {
            seen.lock().expect("seen lock").push(request);
            reply
        })
    })
}

/// 回显 `runtime_id` 的「正常」ack（上游 handler 的典型形状）。
///
/// 返回值必须包成 `Result`：这是 [`mc_ws::frames::HeartbeatHandler`] 的契约。
#[allow(clippy::unnecessary_wraps)]
fn echo_ack(request: &HeartbeatRequest) -> Result<Option<DaemonHeartbeatAckPayload>, String> {
    Ok(Some(DaemonHeartbeatAckPayload {
        runtime_id: request.runtime_id.clone(),
        status: "ok".to_owned(),
        ..DaemonHeartbeatAckPayload::default()
    }))
}

fn heartbeat_frame(runtime_id: &str, supports_batch_import: bool) -> String {
    serde_json::json!({
        "type": "daemon:heartbeat",
        "payload": {
            "runtime_id": runtime_id,
            "supports_batch_import": supports_batch_import,
        }
    })
    .to_string()
}

#[tokio::test]
async fn heartbeat_is_answered_with_the_handler_payload() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::new();
    hub.set_heartbeat_handler(recording_handler(Arc::clone(&seen), |request| {
        Ok(Some(DaemonHeartbeatAckPayload {
            runtime_id: request.runtime_id.clone(),
            status: "ok".to_owned(),
            server_capabilities: vec!["rpc-v1".to_owned()],
            ..DaemonHeartbeatAckPayload::default()
        }))
    }));
    let server = TestServer::start(hub).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1", "rt-2"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    send_text(&mut client, &heartbeat_frame("rt-1", true)).await;
    // 载荷键序 = 字典序（`docs/16` §11.5）：runtime_gone 省略、server_capabilities 在。
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_id":"rt-1","server_capabilities":["rpc-v1"],"status":"ok"}}"#
    );

    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].runtime_id, "rt-1");
    assert!(seen[0].supports_batch_import);
    assert_eq!(seen[0].identity.daemon_id, "d-1");
    assert_eq!(seen[0].identity.runtime_ids, vec!["rt-1", "rt-2"]);
}

#[tokio::test]
async fn default_ack_serializes_only_the_echoed_fields() {
    let hub = Hub::new();
    hub.set_heartbeat_handler(recording_handler(
        Arc::new(Mutex::new(Vec::new())),
        echo_ack,
    ));
    let server = TestServer::start(hub).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    send_text(&mut client, &heartbeat_frame("rt-1", false)).await;
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_id":"rt-1","status":"ok"}}"#
    );
}

#[tokio::test]
async fn heartbeat_for_a_runtime_outside_the_scope_is_ignored() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::new();
    hub.set_heartbeat_handler(recording_handler(Arc::clone(&seen), |request| {
        Ok(Some(DaemonHeartbeatAckPayload {
            runtime_id: request.runtime_id.clone(),
            status: "ok".to_owned(),
            ..DaemonHeartbeatAckPayload::default()
        }))
    }));
    let server = TestServer::start(hub).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    send_text(&mut client, &heartbeat_frame("rt-9", false)).await;
    send_text(&mut client, &heartbeat_frame("", false)).await;
    assert!(
        quiet_for(&mut client, QUIET).await,
        "越权/空 runtime 不该回 ack"
    );

    // 屏障：再发一次合法心跳，等它的 ack。读泵是顺序的，所以收到这一帧时上面两次
    // 一定已经处理完；若它们曾误回 ack，FIFO 队列会把那些 ack 排在前面。
    send_text(&mut client, &heartbeat_frame("rt-1", false)).await;
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_id":"rt-1","status":"ok"}}"#
    );
    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].runtime_id, "rt-1");
}

#[tokio::test]
async fn heartbeat_without_a_handler_is_ignored() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    send_text(&mut client, &heartbeat_frame("rt-1", true)).await;
    assert!(
        quiet_for(&mut client, QUIET).await,
        "没装 handler 不该回 ack"
    );
    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:task_available","payload":{"runtime_id":"rt-1","task_id":"task-9"}}"#
    );
}

#[tokio::test]
async fn heartbeat_missing_runtime_id_is_ignored() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let hub = Hub::new();
    hub.set_heartbeat_handler(Arc::new(move |request: HeartbeatRequest| {
        let counter = Arc::clone(&counter);
        let ack = echo_ack(&request);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            ack
        })
    }));
    let server = TestServer::start(hub).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    // 载荷解不开（runtime_id 类型错）与空 runtime_id 都不进 handler。
    send_text(
        &mut client,
        r#"{"type":"daemon:heartbeat","payload":{"runtime_id":42}}"#,
    )
    .await;
    send_text(
        &mut client,
        r#"{"type":"daemon:heartbeat","payload":{"runtime_id":""}}"#,
    )
    .await;
    assert!(
        quiet_for(&mut client, QUIET).await,
        "解不开/空 runtime 不该回 ack"
    );

    // 屏障帧：合法心跳的 ack 到了，前面两帧才算确定处理完。
    send_text(
        &mut client,
        r#"{"type":"daemon:heartbeat","payload":{"runtime_id":"rt-1","supports_batch_import":true}}"#,
    )
    .await;
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_id":"rt-1","status":"ok"}}"#
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn handler_none_and_handler_error_leave_the_client_without_an_ack() {
    let hub = Hub::new();
    hub.set_heartbeat_handler(recording_handler(Arc::new(Mutex::new(Vec::new())), |_| {
        Ok(None)
    }));
    let server = TestServer::start(hub).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    send_text(&mut client, &heartbeat_frame("rt-1", false)).await;
    assert!(quiet_for(&mut client, QUIET).await, "Ok(None) 不该回 ack");

    server
        .hub
        .set_heartbeat_handler(recording_handler(Arc::new(Mutex::new(Vec::new())), |_| {
            Err("pop pending work failed".to_owned())
        }));
    send_text(&mut client, &heartbeat_frame("rt-1", false)).await;
    assert!(
        quiet_for(&mut client, QUIET).await,
        "handler 报错不该回 ack"
    );

    // 屏障：换成会回 ack 的 handler，再发一次。若上面那次误回过 ack，客户端读到的
    // 第一帧就会是它（FIFO）而不是这条 ack。
    server.hub.set_heartbeat_handler(recording_handler(
        Arc::new(Mutex::new(Vec::new())),
        echo_ack,
    ));
    send_text(&mut client, &heartbeat_frame("rt-1", false)).await;
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_id":"rt-1","status":"ok"}}"#
    );
    assert_eq!(server.hub.connection_count(), 1);
}

#[tokio::test]
async fn heartbeat_after_runtime_gone_is_no_longer_in_scope() {
    let hub = Hub::new();
    hub.set_heartbeat_handler(recording_handler(
        Arc::new(Mutex::new(Vec::new())),
        echo_ack,
    ));
    let server = TestServer::start(hub).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    // runtime 失效 → 连接还在，但 scope 里已经没有它了。
    assert_eq!(
        server.hub.notify_runtime_gone("rt-1"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_gone":true,"runtime_id":"rt-1","status":"runtime_gone"}}"#
    );
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);

    send_text(&mut client, &heartbeat_frame("rt-1", false)).await;
    assert!(
        quiet_for(&mut client, QUIET).await,
        "失效后的心跳不该回 ack"
    );
    assert_eq!(server.hub.connection_count(), 1);
}
