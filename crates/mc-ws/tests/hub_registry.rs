//! 注册表与扇出面：身份 → 三个索引 → **只送到该送的人**（真 socket）。
//!
//! 上游对应 `daemonws.Hub.register/unregister/notifyFrame/notifyWorkspaceFrame/notifyUserFrame`
//! 与 `HandleWebSocket` 的身份校验分支。

mod hub_support;

use hub_support::*;
use mc_ws::hub::{DeliveryOutcome, Hub};

const WORKSPACES_CHANGED: &str = r#"{"type":"daemon:workspaces_changed","payload":{}}"#;

/// 期望的线上字节（载荷键序 = 字典序，见 `docs/16` §11.5）。
fn task_available(runtime_id: &str, task_id: &str) -> String {
    format!(
        r#"{{"type":"daemon:task_available","payload":{{"runtime_id":"{runtime_id}","task_id":"{task_id}"}}}}"#
    )
}

fn pending_work(runtime_id: &str, kind: &str) -> String {
    format!(
        r#"{{"type":"daemon:pending_work","payload":{{"kind":"{kind}","runtime_id":"{runtime_id}"}}}}"#
    )
}

fn profiles_changed(workspace_id: &str, profile_id: &str) -> String {
    format!(
        r#"{{"type":"daemon:runtime_profiles_changed","payload":{{"runtime_profile_id":"{profile_id}","workspace_id":"{workspace_id}"}}}}"#
    )
}

#[tokio::test]
async fn missing_identity_is_rejected_before_upgrade() {
    let server = TestServer::start(Hub::new()).await;

    // 合法升级请求 + 空身份 → hub 自己回 400，不升级。
    let response = raw_upgrade(server.addr, &[]).await;
    assert_eq!(response.status, 400);
    assert_eq!(
        response.header("content-type").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        response.body,
        r#"{"error":"runtime_ids or user identity required"}"#
    );
    assert_eq!(server.hub.connection_count(), 0);

    // 只给 workspace 不给 runtime/user 也算空身份（workspace 不是身份来源）。
    let response = raw_upgrade(server.addr, &[(H_WORKSPACE_ID, "ws-1")]).await;
    assert_eq!(response.status, 400);
    assert_eq!(
        response.body,
        r#"{"error":"runtime_ids or user identity required"}"#
    );

    // 有 runtime 就升级（101）。
    let response = raw_upgrade(server.addr, &[(H_RUNTIME_IDS, "rt-1, rt-2")]).await;
    assert_eq!(response.status, 101);
    assert!(response.header("sec-websocket-accept").is_some());
    wait_until("升级后注册", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 1);
    assert_eq!(server.hub.runtime_connection_count("rt-2"), 1);
}

#[tokio::test]
async fn upgrade_registers_every_index_dimension() {
    let server = TestServer::start(Hub::new()).await;
    let _daemon = server
        .connect(
            &TestIdentity::daemon("d-1", &["rt-1", "rt-2"])
                .with_workspaces(&["ws-1", "ws-2"])
                .with_workspace("ws-legacy"),
        )
        .await;
    wait_until("daemon 注册", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 1);
    assert_eq!(server.hub.runtime_connection_count("rt-2"), 1);
    // 多工作区字段优先：legacy 字段被忽略。
    assert_eq!(server.hub.workspace_connection_count("ws-1"), 1);
    assert_eq!(server.hub.workspace_connection_count("ws-2"), 1);
    assert_eq!(server.hub.workspace_connection_count("ws-legacy"), 0);

    let _user = server
        .connect(&TestIdentity::user("u-1").with_workspace("ws-9"))
        .await;
    wait_until("user 注册", || server.hub.connection_count() == 2).await;
    assert_eq!(server.hub.user_connection_count("u-1"), 1);
    assert_eq!(server.hub.workspace_connection_count("ws-9"), 1);
    // 只给 user 的连接不占任何 runtime 索引。
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 1);
}

#[tokio::test]
async fn runtime_scoped_notification_reaches_only_matching_connections() {
    let server = TestServer::start(Hub::new()).await;
    let mut a = server
        .connect(&TestIdentity::daemon("d-a", &["rt-1"]).with_workspace("ws-1"))
        .await;
    let mut b = server
        .connect(&TestIdentity::daemon("d-b", &["rt-1", "rt-2"]))
        .await;
    let mut c = server
        .connect(&TestIdentity::daemon("d-c", &["rt-3"]))
        .await;
    wait_until("三条连接就绪", || server.hub.connection_count() == 3).await;
    assert_eq!(server.hub.runtime_connection_count("rt-1"), 2);

    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut a, WAIT).await,
        task_available("rt-1", "task-9")
    );
    assert_eq!(
        expect_text(&mut b, WAIT).await,
        task_available("rt-1", "task-9")
    );
    assert!(quiet_for(&mut c, QUIET).await, "rt-3 不该收到 rt-1 的通知");

    // rt-2 只属于 b。
    assert_eq!(
        server.hub.notify_task_available("rt-2", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut b, WAIT).await,
        task_available("rt-2", "task-9")
    );
    assert!(quiet_for(&mut a, QUIET).await);

    assert_eq!(
        server.hub.notify_pending_work("rt-1", "local_skill_import"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut a, WAIT).await,
        pending_work("rt-1", "local_skill_import")
    );

    // 没有连接的 runtime / 空 key 都是 miss 且不产生帧。
    assert_eq!(
        server.hub.notify_task_available("rt-nope", "task-9"),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_pending_work("rt-nope", "kind"),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_task_available("", "task-9"),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut a, QUIET).await);
}

#[tokio::test]
async fn workspace_scoped_notification_reaches_only_that_workspace() {
    let server = TestServer::start(Hub::new()).await;
    let mut ws_one = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    let mut ws_two = server
        .connect(&TestIdentity::daemon("d-2", &["rt-2"]).with_workspace("ws-2"))
        .await;
    let mut user = server.connect(&TestIdentity::user("u-1")).await;
    wait_until("三条连接就绪", || server.hub.connection_count() == 3).await;

    assert_eq!(
        server.hub.notify_runtime_profiles_changed("ws-1", "prof-2"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut ws_one, WAIT).await,
        profiles_changed("ws-1", "prof-2")
    );
    assert!(
        quiet_for(&mut ws_two, QUIET).await,
        "ws-2 不该收到 ws-1 的变更"
    );
    assert!(
        quiet_for(&mut user, QUIET).await,
        "纯 user 连接不订阅 workspace 面"
    );

    assert_eq!(
        server
            .hub
            .notify_runtime_profiles_changed("ws-none", "prof-2"),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_runtime_profiles_changed("", "prof-2"),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut ws_one, QUIET).await);
}

#[tokio::test]
async fn legacy_workspace_field_becomes_the_scope_when_multi_workspace_is_absent() {
    let server = TestServer::start(Hub::new()).await;
    let mut legacy = server
        .connect(&TestIdentity::daemon("d-legacy", &["rt-1"]).with_workspace("ws-8"))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.workspace_connection_count("ws-8"), 1);

    assert_eq!(
        server.hub.notify_runtime_profiles_changed("ws-8", "prof-2"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut legacy, WAIT).await,
        profiles_changed("ws-8", "prof-2")
    );
}

#[tokio::test]
async fn multi_workspace_scope_is_deduped_and_all_keys_are_indexed() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(
            &TestIdentity::daemon("d-multi", &["rt-1"]).with_workspaces(&["ws-1", "ws-1", "ws-2"]),
        )
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;
    assert_eq!(server.hub.workspace_connection_count("ws-1"), 1);
    assert_eq!(server.hub.workspace_connection_count("ws-2"), 1);

    assert_eq!(
        server.hub.notify_runtime_profiles_changed("ws-2", "prof-2"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        profiles_changed("ws-2", "prof-2")
    );
}

#[tokio::test]
async fn user_scoped_notification_reaches_only_that_user() {
    let server = TestServer::start(Hub::new()).await;
    let mut mine = server.connect(&TestIdentity::user("u-1")).await;
    let mut other = server.connect(&TestIdentity::user("u-2")).await;
    let mut daemon = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("三条连接就绪", || server.hub.connection_count() == 3).await;

    assert_eq!(
        server.hub.notify_workspaces_changed("u-1"),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut mine, WAIT).await, WORKSPACES_CHANGED);
    assert!(quiet_for(&mut other, QUIET).await);
    assert!(quiet_for(&mut daemon, QUIET).await);

    // 多租户 daemon（带 user_id 的 daemon 连接）也吃 user 面。
    let mut tenant = server
        .connect(&TestIdentity {
            user_id: "u-1".to_owned(),
            ..TestIdentity::daemon("d-tenant", &["rt-9"])
        })
        .await;
    wait_until("第四条连接就绪", || {
        server.hub.connection_count() == 4
    })
    .await;
    assert_eq!(server.hub.user_connection_count("u-1"), 2);
    assert_eq!(
        server.hub.notify_workspaces_changed("u-1"),
        DeliveryOutcome::hit()
    );
    assert_eq!(expect_text(&mut mine, WAIT).await, WORKSPACES_CHANGED);
    assert_eq!(expect_text(&mut tenant, WAIT).await, WORKSPACES_CHANGED);
    assert!(quiet_for(&mut other, QUIET).await);

    assert_eq!(
        server.hub.notify_workspaces_changed("u-none"),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_workspaces_changed(""),
        DeliveryOutcome::miss()
    );
}

#[tokio::test]
async fn invalid_and_unknown_frames_are_ignored_without_dropping_the_connection() {
    let server = TestServer::start(Hub::new()).await;
    let mut client = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    // 非法 JSON、JSON 非对象、未知类型、心跳（未装 handler）都只记日志。
    send_text(&mut client, "not json").await;
    send_text(&mut client, "[1,2,3]").await;
    send_text(
        &mut client,
        r#"{"type":"daemon:something_unknown","payload":{}}"#,
    )
    .await;
    send_text(
        &mut client,
        r#"{"type":"daemon:heartbeat","payload":{"runtime_id":"rt-1"}}"#,
    )
    .await;
    send_frame(
        &mut client,
        tokio_tungstenite::tungstenite::Message::Binary(vec![1, 2, 3]),
    )
    .await;
    // 客户端 ping：由传输层自动回 pong（tungstenite 行为），不进分派。
    send_frame(
        &mut client,
        tokio_tungstenite::tungstenite::Message::Ping(Vec::new()),
    )
    .await;

    // 连接仍然活着，且扇出照常工作。
    assert_eq!(server.hub.connection_count(), 1);
    assert_eq!(
        server.hub.notify_task_available("rt-1", "task-9"),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut client, WAIT).await,
        task_available("rt-1", "task-9")
    );
    assert_eq!(server.hub.connection_count(), 1);
}
