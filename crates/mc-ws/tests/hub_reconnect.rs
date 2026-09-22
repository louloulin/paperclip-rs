//! 连接生命周期（注册/注销）与 relay 回环面：`deliver_daemon_runtime` 的分派、去重、
//! 逐字节转发。
//!
//! 上游对应 `daemonws.Hub.register/unregister`、`DeliverDaemonRuntime`（`hub.go:577`）、
//! `notifyFrame`（含每连接 `eventID` 去重）与 `invalidateRuntime`（`hub.go:545`）。

mod hub_support;

use hub_support::*;
use mc_ws::hub::{DeliveryOutcome, Hub};

/// 本文件的帧一律以「relay 转发过来的原文」身份使用 —— hub 不重新编码它们。
fn task_available(runtime_id: &str, task_id: &str) -> String {
    format!(
        r#"{{"type":"daemon:task_available","payload":{{"runtime_id":"{runtime_id}","task_id":"{task_id}"}}}}"#
    )
}

fn profiles_changed(workspace_id: &str, profile_id: &str) -> String {
    format!(
        r#"{{"type":"daemon:runtime_profiles_changed","payload":{{"runtime_profile_id":"{profile_id}","workspace_id":"{workspace_id}"}}}}"#
    )
}

fn pending_work(runtime_id: &str, kind: &str) -> String {
    format!(
        r#"{{"type":"daemon:pending_work","payload":{{"kind":"{kind}","runtime_id":"{runtime_id}"}}}}"#
    )
}

const WORKSPACES_CHANGED: &str = r#"{"type":"daemon:workspaces_changed","payload":{}}"#;

fn runtime_gone(runtime_id: &str) -> String {
    format!(
        r#"{{"type":"daemon:heartbeat_ack","payload":{{"runtime_gone":true,"runtime_id":"{runtime_id}","status":"runtime_gone"}}}}"#
    )
}

#[tokio::test]
async fn reconnect_replaces_the_registration() {
    let server = TestServer::start(Hub::new()).await;

    let first = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    wait_until("首次注册", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 1);

    drop(first);
    wait_until("注销", || server.hub.connection_count() == 0).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);
    assert_eq!(server.hub.workspace_connection_count("ws-1"), 0);
    // 注销后没人收，扇出退化成 miss（而不是投给旧连接）。
    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::miss()
    );

    let mut second = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    wait_until("重连注册", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 1);
    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut second, WAIT).await,
        task_available("rt-1", "task-9")
    );
}

#[tokio::test]
async fn disconnect_clears_workspace_and_user_indices_too() {
    let server = TestServer::start(Hub::new()).await;
    let daemon = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1", "rt-2"]).with_workspaces(&["ws-1", "ws-2"]))
        .await;
    let user = server
        .connect(&TestIdentity {
            user_id: "u-1".to_owned(),
            ..TestIdentity::daemon("d-2", &["rt-3"])
        })
        .await;
    wait_until("两条连接注册", || server.hub.connection_count() == 2).await;

    drop(daemon);
    wait_until("daemon 注销", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);
    assert_eq!(server.hub.runtime_connection_count("rt-2"), 0);
    assert_eq!(server.hub.workspace_connection_count("ws-1"), 0);
    assert_eq!(server.hub.workspace_connection_count("ws-2"), 0);
    assert_eq!(server.hub.user_connection_count("u-1"), 1);

    drop(user);
    wait_until("全部注销", || server.hub.connection_count() == 0).await;
    assert_eq!(server.hub.runtime_connection_count("rt-3"), 0);
    assert_eq!(server.hub.user_connection_count("u-1"), 0);
    assert_eq!(
        server.hub.notify_workspaces_changed("u-1"),
        DeliveryOutcome::miss()
    );
}

#[tokio::test]
async fn relay_frames_are_dispatched_by_frame_type() {
    let server = TestServer::start(Hub::new()).await;
    let mut daemon_one = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    let mut daemon_two = server
        .connect(&TestIdentity::daemon("d-2", &["rt-2"]))
        .await;
    let mut user = server
        .connect(&TestIdentity::user("u-1").with_workspace("ws-1"))
        .await;
    wait_until("三条连接注册", || server.hub.connection_count() == 3).await;

    // task_available → runtime 索引（载荷里的 runtime_id）。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &task_available("rt-1", "task-9"), ""),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut daemon_one, WAIT).await,
        task_available("rt-1", "task-9")
    );
    assert!(quiet_for(&mut daemon_two, QUIET).await);
    assert!(quiet_for(&mut user, QUIET).await);

    // runtime_profiles_changed → workspace 索引（载荷里的 workspace_id，忽略 scope_id）。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("u-1", &profiles_changed("ws-1", "prof-2"), ""),
        DeliveryOutcome::hit()
    );
    let expected = profiles_changed("ws-1", "prof-2");
    assert_eq!(expect_text(&mut daemon_one, WAIT).await, expected);
    assert_eq!(expect_text(&mut user, WAIT).await, expected);
    assert!(quiet_for(&mut daemon_two, QUIET).await);

    // workspaces_changed → user 索引，key 来自 relay 的 scope_id。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("u-1", WORKSPACES_CHANGED, ""),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut user, WAIT).await, WORKSPACES_CHANGED);
    assert!(quiet_for(&mut daemon_one, QUIET).await);

    // pending_work → runtime 索引。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &pending_work("rt-2", "local_skill_import"), ""),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut daemon_two, WAIT).await,
        pending_work("rt-2", "local_skill_import")
    );

    // runtime_gone 形状的 heartbeat_ack → 失效：连接还在，scope 里少了一个 runtime。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &runtime_gone("rt-1"), "evt-gone"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut daemon_one, WAIT).await,
        runtime_gone("rt-1")
    );
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);
    assert_eq!(server.hub.connection_count(), 3);
}

#[tokio::test]
async fn relay_forwards_the_exact_bytes_it_received() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    // 键序刻意非字典序、且带空白 —— 若 hub 做了重编码就会被改写。
    let raw = "{ \"payload\" : {\"task_id\":\"task-9\",\"runtime_id\":\"rt-1\"} , \"type\":\"daemon:task_available\" }";
    assert_eq!(
        server.hub.deliver_daemon_runtime("", raw, ""),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut client, WAIT).await, raw);
}

#[tokio::test]
async fn relay_event_dedup_is_per_connection_and_keyed_by_event_id() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    let first = task_available("rt-1", "task-9");
    assert_eq!(
        server.hub.deliver_daemon_runtime("", &first, "evt-1"),
        DeliveryOutcome::hit()
    );
    let again = server.hub.deliver_daemon_runtime("", &first, "evt-1");
    assert!(!again.delivered);
    assert!(
        again.deduped,
        "同一 event_id 第二次是「去重」而不是「投递失败」"
    );

    // 屏障帧：新 event_id 的帧先到才说明上面那次重复确实没入队（FIFO）。
    let second = task_available("rt-1", "task-10");
    assert_eq!(
        server.hub.deliver_daemon_runtime("", &second, "evt-2"),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut client, WAIT).await, first);
    assert_eq!(expect_text(&mut client, WAIT).await, second);

    // 空 event_id 按上游语义绕过去重：同一帧投两次就收两次。
    let third = task_available("rt-1", "task-11");
    assert_eq!(
        server.hub.deliver_daemon_runtime("", &third, ""),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        server.hub.deliver_daemon_runtime("", &third, ""),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut client, WAIT).await, third);
    assert_eq!(expect_text(&mut client, WAIT).await, third);
}

#[tokio::test]
async fn relay_ignores_invalid_or_unroutable_frames() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    let cases = [
        // 不是 JSON / 不是对象
        "not json".to_owned(),
        "[]".to_owned(),
        // 未知类型
        r#"{"type":"daemon:something_unknown","payload":{}}"#.to_owned(),
        // 载荷缺 key（空串）
        r#"{"type":"daemon:task_available","payload":{"runtime_id":"","task_id":"t"}}"#.to_owned(),
        r#"{"type":"daemon:pending_work","payload":{"runtime_id":"","kind":"k"}}"#.to_owned(),
        r#"{"type":"daemon:runtime_profiles_changed","payload":{"workspace_id":"","runtime_profile_id":"p"}}"#
            .to_owned(),
        // 载荷类型错
        r#"{"type":"daemon:task_available","payload":{"runtime_id":7,"task_id":"t"}}"#.to_owned(),
        // 心跳 ack 但不是 runtime_gone
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_id":"rt-1","status":"ok"}}"#.to_owned(),
        // runtime_gone=true 但 status 不对 / runtime_id 空
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_gone":true,"runtime_id":"rt-1","status":"ok"}}"#
            .to_owned(),
        r#"{"type":"daemon:heartbeat_ack","payload":{"runtime_gone":true,"runtime_id":"","status":"runtime_gone"}}"#
            .to_owned(),
    ];
    for frame in &cases {
        let outcome = server.hub.deliver_daemon_runtime("", frame, "evt-x");
        assert_eq!(outcome, DeliveryOutcome::miss(), "帧不该被路由：{frame}");
    }
    // workspaces_changed 用 scope_id 做 key：scope 为空 → miss。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", WORKSPACES_CHANGED, "evt-x"),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut client, QUIET).await);

    // 连接没被这些废帧影响：正常帧照收。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &task_available("rt-1", "task-9"), "evt-ok"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        task_available("rt-1", "task-9")
    );
    assert_eq!(server.hub.connection_count(), 1);
}

#[tokio::test]
async fn runtime_gone_dedup_survives_an_event_that_raced_ahead_of_registration() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    // 第一跳：有连接 → 投递 + 标记 hub 级去重 + 摘掉 runtime。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &runtime_gone("rt-1"), "evt-gone"),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut client, WAIT).await, runtime_gone("rt-1"));

    // 第二跳：同一 event_id 的回环，此时已无连接 → hub 级去重直接吞掉，不消耗事件。
    let dup = server
        .hub
        .deliver_daemon_runtime("", &runtime_gone("rt-1"), "evt-gone");
    assert!(!dup.delivered);
    assert!(dup.deduped);
    assert!(quiet_for(&mut client, QUIET).await);

    // 「抢在注册之前」的分支：无连接 + 该 event_id 没见过 → 撤销标记并报 miss，
    // 这样它的回环还能失效刚注册上来的新 scope。
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &runtime_gone("rt-2"), "evt-gone-2"),
        DeliveryOutcome::miss()
    );
    let mut rejoined = server
        .connect(&TestIdentity::daemon("d-2", &["rt-2"]))
        .await;
    wait_until("rt-2 注册", || {
        server.hub.runtime_connection_count("rt-2") == 1
    })
    .await;
    assert_eq!(
        server
            .hub
            .deliver_daemon_runtime("", &runtime_gone("rt-2"), "evt-gone-2"),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut rejoined, WAIT).await, runtime_gone("rt-2"));
    assert_eq!(server.hub.runtime_connection_count("rt-2"), 0);
}

#[tokio::test]
async fn direct_runtime_gone_notification_matches_the_relay_shape() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接注册", || server.hub.connection_count() == 1).await;

    // 直接调用（本地进程内）与 relay 回环必须产生同样的字节。
    assert_eq!(
        server.hub.notify_runtime_gone("rt-1"),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut client, WAIT).await, runtime_gone("rt-1"));
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 0);

    // 空 runtime_id / 未知 runtime → miss，且不产生帧。
    assert_eq!(server.hub.notify_runtime_gone(""), DeliveryOutcome::miss());
    assert_eq!(
        server.hub.notify_runtime_gone("rt-unknown"),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut client, QUIET).await);
    assert_eq!(server.hub.connection_count(), 1);
}
