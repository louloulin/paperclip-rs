//! 用户面事件帧：`chat:done` / `task:queued` / `agent:status` 的**投递面**（真 socket）。
//!
//! 上游把 daemon 面与用户面分成两个传输层（`daemonws.Hub` vs `events.Bus` + 工作区订阅者），
//! 本仓只有一条 `/api/daemon/ws` 连接面（`docs/32` D-4）⇒ 用户面通知必须在 hub 内部把
//! **daemon 面连接**排除掉。本文件锁的就是这条过滤：没有它，`chat:done` 的正文会顺着
//! `Index::Workspace` 投给同一工作区的 daemon 面连接。

mod hub_support;

use hub_support::*;
use mc_daemon_proto::messages::chat::ChatQuickAction;
use mc_ws::frames::{
    AgentStatusPayload, ChatDonePayload, ChatMessagePayload, ChatQuickActionsPayload,
    TaskQueuedPayload,
};
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

/// 派发家族的三个新帧（M4-4-fu / LUM-1600）走**同一条**用户面过滤：只有同工作区的
/// 用户连接收到，daemon 面连接与其他工作区都被排除 —— 与 `chat:done` 一条不差。
///
/// 帧字节逐字断言（键序 = 字典序，见 `docs/16` §11.5）：用户面帧的键集是契约的一部分，
/// `task_id` / `failed` 两个 `omitempty` 键的**缺席**与 `quick_actions: []` 的**在场**
/// 都是客户端分支的依据。
#[tokio::test]
async fn dispatch_family_frames_share_the_user_face_filter() {
    let server = TestServer::start(Hub::new()).await;
    let mut user = server
        .connect(&TestIdentity::user("u-1").with_workspace("ws-1"))
        .await;
    // 同一工作区的 daemon 面连接：`workspace_id` 一致、`user_id` 为空 ⇒ 必须被排除。
    let mut daemon = server
        .connect(&TestIdentity::daemon("d-1", &["rt-1"]).with_workspace("ws-1"))
        .await;
    let mut other = server
        .connect(&TestIdentity::user("u-2").with_workspace("ws-2"))
        .await;
    wait_until("三条连接就绪", || server.hub.connection_count() == 3).await;

    // 1) `chat:message`（上游 `publishChat(EventChatMessage)`，`chat.go:992`）。
    let message = ChatMessagePayload {
        chat_session_id: "cs-1".to_owned(),
        message_id: "msg-1".to_owned(),
        role: "user".to_owned(),
        content: "你好".to_owned(),
        task_id: "task-1".to_owned(),
        created_at: "2026-01-02T03:04:05Z".to_owned(),
    };
    assert_eq!(
        server.hub.notify_chat_message("ws-1", &message),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut user, WAIT).await,
        r#"{"type":"chat:message","payload":{"chat_session_id":"cs-1","content":"你好","created_at":"2026-01-02T03:04:05Z","message_id":"msg-1","role":"user","task_id":"task-1"}}"#
    );
    assert!(
        quiet_for(&mut daemon, QUIET).await,
        "chat:message 正文不能投给 daemon 面连接"
    );
    assert!(
        quiet_for(&mut other, QUIET).await,
        "ws-2 不该收到 ws-1 的帧"
    );

    // 2) `chat:quick_actions`（上游 `service/chat_quick_actions.go:285`）：`quick_actions`
    //    恒在（可为空数组）、`failed` 只在失败收敛时出现。
    let actions = ChatQuickActionsPayload {
        chat_session_id: "cs-1".to_owned(),
        task_id: "task-1".to_owned(),
        message_id: "msg-2".to_owned(),
        quick_actions: vec![ChatQuickAction {
            label: "换个说法".to_owned(),
            prompt: "换一种说法重讲".to_owned(),
            primary: true,
        }],
        failed: false,
    };
    assert_eq!(
        server.hub.notify_chat_quick_actions("ws-1", &actions),
        DeliveryOutcome::hit()
    );
    let text = expect_text(&mut user, WAIT).await;
    assert_eq!(
        text,
        r#"{"type":"chat:quick_actions","payload":{"chat_session_id":"cs-1","message_id":"msg-2","quick_actions":[{"label":"换个说法","primary":true,"prompt":"换一种说法重讲"}],"task_id":"task-1"}}"#
    );
    assert!(!text.contains("failed"), "成功收敛不该有 failed 键：{text}");
    assert!(quiet_for(&mut daemon, QUIET).await);
    assert!(quiet_for(&mut other, QUIET).await);

    // 失败收敛：`failed: true` 出现，空数组仍然是 `[]` —— 这是解开客户端骨架屏的终态。
    let failed = ChatQuickActionsPayload {
        chat_session_id: "cs-1".to_owned(),
        task_id: "task-1".to_owned(),
        message_id: "msg-2".to_owned(),
        quick_actions: Vec::new(),
        failed: true,
    };
    assert_eq!(
        server.hub.notify_chat_quick_actions("ws-1", &failed),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut user, WAIT).await,
        r#"{"type":"chat:quick_actions","payload":{"chat_session_id":"cs-1","failed":true,"message_id":"msg-2","quick_actions":[],"task_id":"task-1"}}"#
    );
    assert!(quiet_for(&mut daemon, QUIET).await);

    // 3) `task:cancelled`（上游 `BroadcastCancelledTasks`）：与 `task:queued` 同一份
    //    `taskEvent` 键集，只有 `status` 不同；chat 任务的 `issue_id` 是空串（键仍在）。
    let cancelled = TaskQueuedPayload {
        task_id: "task-9".to_owned(),
        agent_id: "ag-1".to_owned(),
        issue_id: String::new(),
        status: "cancelled".to_owned(),
        chat_session_id: Some("cs-1".to_owned()),
    };
    assert_eq!(
        server.hub.notify_task_cancelled("ws-1", &cancelled),
        DeliveryOutcome::hit()
    );
    assert_eq!(
        expect_text(&mut user, WAIT).await,
        r#"{"type":"task:cancelled","payload":{"agent_id":"ag-1","chat_session_id":"cs-1","issue_id":"","status":"cancelled","task_id":"task-9"}}"#
    );
    assert!(quiet_for(&mut daemon, QUIET).await);
    assert!(quiet_for(&mut other, QUIET).await);

    // 空工作区 = miss，且不产生任何帧（上游 `notifyWorkspaceFrame` 的 `""` 分支）。
    assert_eq!(
        server.hub.notify_chat_message("", &message),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_chat_quick_actions("", &actions),
        DeliveryOutcome::miss()
    );
    assert_eq!(
        server.hub.notify_task_cancelled("", &cancelled),
        DeliveryOutcome::miss()
    );
    assert!(quiet_for(&mut user, QUIET).await);
}
