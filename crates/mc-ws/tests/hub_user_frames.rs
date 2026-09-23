//! 用户面事件帧：`chat:done` / `task:queued` / `agent:status` 的**投递面**（真 socket）。
//!
//! 上游把 daemon 面与用户面分成两个传输层（`daemonws.Hub` vs `events.Bus` + 工作区订阅者），
//! 本仓只有一条 `/api/daemon/ws` 连接面（`docs/32` D-4）⇒ 用户面通知必须在 hub 内部把
//! **daemon 面连接**排除掉。本文件锁的就是这条过滤：没有它，`chat:done` 的正文会顺着
//! `Index::Workspace` 投给同一工作区的 daemon 面连接。

mod hub_support;

use hub_support::*;
use mc_ws::frames::{AgentStatusPayload, ChatDonePayload, TaskQueuedPayload};
use mc_ws::hub::{DeliveryOutcome, Hub};
use serde_json::json;

#[tokio::test]
async fn user_frames_reach_only_user_connections_of_that_workspace() {
    let server = TestServer::start(Hub::new()).await;
    // 同一工作区的**用户**连接：受众。
    let mut user = server
        .connect(&TestIdentity::user("u-1").with_workspace("ws-1"))
        .await;
    // 同一工作区的 **daemon 面**连接：`workspace_id` 一致、`user_id` 为空 ⇒ 必须被排除。
    let mut daemon = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    // 另一个工作区的用户：不在受众内。
    let mut other = server
        .connect(&TestIdentity::user("u-2").with_workspace("ws-2"))
        .await;
    wait_until("三条连接就绪", || server.hub.connection_count() == 3).await;

    let chat_done = ChatDonePayload {
        chat_session_id: "cs-1".to_owned(),
        task_id: "task-1".to_owned(),
        content: Some("上游原文".to_owned()),
        quick_actions_pending: true,
        ..ChatDonePayload::default()
    };
    assert_eq!(
        server.hub.notify_chat_done("ws-1", &chat_done),
        DeliveryOutcome::hit()
    );
    let text = expect_text(&mut user, WAIT).await;
    assert!(text.starts_with(r#"{"type":"chat:done""#), "{text}");
    assert!(text.contains("上游原文"), "{text}");
    // daemon 面连接拿不到正文（这条断言就是过滤存在的理由）。
    assert!(
        quiet_for(&mut daemon, QUIET).await,
        "daemon 面连接不该收到 chat:done 正文"
    );
    assert!(
        quiet_for(&mut other, QUIET).await,
        "ws-2 不该收到 ws-1 的帧"
    );

    let queued = TaskQueuedPayload {
        task_id: "task-2".to_owned(),
        agent_id: "ag-1".to_owned(),
        issue_id: "issue-1".to_owned(),
        status: "queued".to_owned(),
        chat_session_id: Some("cs-1".to_owned()),
    };
    assert_eq!(
        server.hub.notify_task_queued("ws-1", &queued),
        DeliveryOutcome::hit()
    );
    let text = expect_text(&mut user, WAIT).await;
    assert!(text.starts_with(r#"{"type":"task:queued""#), "{text}");
    assert!(quiet_for(&mut daemon, QUIET).await);
    assert!(quiet_for(&mut other, QUIET).await);

    let status = AgentStatusPayload {
        agent: json!({"id": "ag-1", "has_custom_env": true}),
    };
    assert_eq!(
        server.hub.notify_agent_status("ws-1", &status),
        DeliveryOutcome::hit()
    );
    let text = expect_text(&mut user, WAIT).await;
    assert!(text.starts_with(r#"{"type":"agent:status""#), "{text}");
    assert!(quiet_for(&mut daemon, QUIET).await);
    assert!(quiet_for(&mut other, QUIET).await);

    // 空工作区 = miss，且不产生任何帧（上游 `notifyWorkspaceFrame` 的 `""` 分支）。
    assert_eq!(
        server.hub.notify_agent_status("", &status),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_chat_done("", &chat_done),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_task_queued("", &queued),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut user, QUIET).await);

    // 该工作区一个用户都没连 ⇒ miss（不是「投给别的连接」）。
    assert_eq!(
        server.hub.notify_chat_done("ws-none", &chat_done),
        DeliveryOutcome::miss()
    );
}

#[tokio::test]
async fn pure_daemon_workspace_connection_is_not_a_user_audience() {
    let server = TestServer::start(Hub::new()).await;
    // 只有 daemon 面连接：三条用户面通知都必须是 miss —— 空 `user_id` 不算受众。
    let mut daemon = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    wait_until("连接就绪", || server.hub.connection_count() == 1).await;

    let status = AgentStatusPayload {
        agent: json!({"id": "ag-1"}),
    };
    assert_eq!(
        server.hub.notify_agent_status("ws-1", &status),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut daemon, QUIET).await);
}
