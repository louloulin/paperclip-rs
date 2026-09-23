//! `POST /api/issues/:id/tasks/:taskId/cancel`、`POST /api/tasks/:taskId/cancel`。

use axum::http::StatusCode;
use uuid::Uuid;

use crate::support::{
    assert_bad_request, call, error_code, error_message, seed_agent, seed_chat_session, seed_user,
    setup, TaskSeed,
};

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn cancel_issue_task_requires_the_url_issue() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let task = TaskSeed::queued(fx.issue_id).insert(&fx).await;

    let uri = format!("/api/issues/{}/tasks/{task}/cancel", fx.issue_id);
    let (status, body) = call(&app, "POST", &uri, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    assert_eq!(body["cancelled_by"]["type"], "user");
    assert_eq!(body["cancelled_by"]["id"], fx.user_id.to_string());
    assert_eq!(body["cancelled_by"]["name"], "itest-m3b-user");
    assert!(
        body["cancelled_by_comment_change"].is_null(),
        "非评论变更取消不得带上该标记: {body}"
    );
    let persisted: Option<Uuid> =
        sqlx::query_scalar("SELECT cancelled_by_id FROM agent_task_queue WHERE id = $1")
            .bind(task)
            .fetch_one(&fx.pool)
            .await
            .expect("cancelled_by_id");
    assert_eq!(persisted, Some(fx.user_id));

    // 同一个 task 换个 issue 寻址 → 404（不是 403，免得暴露 task 存在性）
    let other_issue = fx.issue("todo").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{other_issue}/tasks/{task}/cancel"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "task");

    // 畸形 task uuid 走「查不到」⇒ 404
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{}/tasks/nope/cancel", fx.issue_id),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_message(&body), "task");

    // 不存在的 issue
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{}/tasks/{task}/cancel", Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn cancel_task_checks_chat_privacy_and_queue_cas() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();

    // issue 任务：直接按 agent 可见性判定
    let issue_task = TaskSeed::queued(fx.issue_id).insert(&fx).await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{issue_task}/cancel"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    assert_eq!(body["cancelled_by"]["name"], "itest-m3b-user");

    // chat 任务的私有性：别人开的会话不能取消
    let other = seed_user(&fx.pool, fx.workspace_id, "member").await;
    let their_session = seed_chat_session(&fx.pool, fx.workspace_id, fx.agent_id, other).await;
    let their_task = TaskSeed {
        issue_id: None,
        status: Some("queued".to_owned()),
        chat_session_id: Some(their_session),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{their_task}/cancel"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_message(&body), "not your task");

    // 自己的会话可以取消
    let my_session = seed_chat_session(&fx.pool, fx.workspace_id, fx.agent_id, fx.user_id).await;
    let my_task = TaskSeed {
        issue_id: None,
        status: Some("queued".to_owned()),
        chat_session_id: Some(my_session),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{my_task}/cancel"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["kind"], "chat");

    // 别人的私有 agent ⇒ 本成员看不见
    let hidden_agent = seed_agent(&fx.pool, fx.workspace_id, fx.runtime_id, Some(other)).await;
    let hidden = TaskSeed {
        issue_id: None,
        status: Some("queued".to_owned()),
        agent_id: Some(hidden_agent),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{hidden}/cancel"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_message(&body), "you do not have access to this agent");

    // chat 队列 CAS
    let cas_session = seed_chat_session(&fx.pool, fx.workspace_id, fx.agent_id, fx.user_id).await;
    let running = TaskSeed {
        issue_id: None,
        status: Some("running".to_owned()),
        started: true,
        chat_session_id: Some(cas_session),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let uri = format!(
        "/api/tasks/{running}/cancel?expected_status=queued&chat_session_id={cas_session}&queue_action=remove"
    );
    let (status, body) = call(&app, "POST", &uri, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "conflict");
    assert_eq!(error_message(&body), "task is no longer queued");

    // 期望的会话不对
    let other_session = seed_chat_session(&fx.pool, fx.workspace_id, fx.agent_id, fx.user_id).await;
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/tasks/{running}/cancel?expected_status=queued&chat_session_id={other_session}&queue_action=remove"
        ),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        error_message(&body),
        "task does not belong to the expected chat session"
    );

    // expected_status 只接受 queued
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{running}/cancel?expected_status=running"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "expected_status must be queued");

    // 缺 chat_session_id / 坏 chat_session_id / 坏 queue_action
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{running}/cancel?expected_status=queued&queue_action=remove"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "invalid chat_session_id");
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{running}/cancel?expected_status=queued&chat_session_id=nope"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "invalid chat_session_id");
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/tasks/{running}/cancel?expected_status=queued&chat_session_id={cas_session}&queue_action=bogus"
        ),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "queue_action must be edit or remove");

    // CAS 命中且仍是 queued ⇒ 成功
    let queued = TaskSeed {
        issue_id: None,
        status: Some("queued".to_owned()),
        chat_session_id: Some(cas_session),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/tasks/{queued}/cancel?expected_status=queued&chat_session_id={cas_session}&queue_action=edit"
        ),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");

    // 未知 task
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/tasks/{}/cancel", Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found");

    fx.cleanup().await;
}
