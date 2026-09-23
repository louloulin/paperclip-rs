//! 5 条 `gc-check` 探测面的端到端用例。
//!
//! 这些端点本身**不删任何东西**：多进程残留清扫由 daemon 侧按「多久没动过」决定，
//! 服务端只回答「这条资源现在是什么状态」。所以断言的是形状与可见性，不是清扫结果。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support;

/// 建一个 `autopilot` + 一行 `autopilot_run`（gc 探针要顺着 run 查回 workspace）。
async fn seed_autopilot_run(pool: &sqlx::PgPool, workspace_id: Uuid, user_id: Uuid) -> Uuid {
    let autopilot_id: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot \
            (workspace_id, title, created_by_type, created_by_id, assignee_id, status) \
         VALUES ($1, 'itest autopilot', 'member', $2, $2, 'active') RETURNING id",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert autopilot");

    sqlx::query_scalar(
        "INSERT INTO autopilot_run(autopilot_id, source, status) \
         VALUES ($1, 'manual', 'completed') RETURNING id",
    )
    .bind(autopilot_id)
    .fetch_one(pool)
    .await
    .expect("insert autopilot_run")
}

/// 5 条 gc-check 各打一次，断言状态码与关键字段。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn gc_check_covers_all_five_probes() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (runtime_id, agent_id, task_id) =
        support::seed_ready_task(&pool, workspace_id, user_id, "m1").await;

    // 1. 批量 issue 探测：回显请求原文 id，并带上 found / status / category。
    let issue_id: Uuid = sqlx::query_scalar("SELECT issue_id FROM agent_task_queue WHERE id = $1")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .expect("task issue");
    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/workspaces/{workspace_id}/issues/gc-check"),
        user_id,
        Some("m1"),
        Some(json!({ "issue_ids": [issue_id.to_string()] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch gc-check: {body}");
    let items = body["issues"].as_array().expect("issues 数组");
    assert_eq!(items.len(), 1, "{body}");
    assert_eq!(items[0]["id"], json!(issue_id.to_string()));
    assert_eq!(items[0]["found"], json!(true));
    assert!(
        items[0]["category"].as_str().is_some_and(|c| !c.is_empty()),
        "category 必须由 status 派生: {}",
        items[0]
    );

    // 2. 单条 issue 探测。
    let (status, body) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/issues/{issue_id}/gc-check"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "issue gc-check: {body}");
    assert!(body["status"].as_str().is_some());
    assert!(body["updated_at"].as_str().is_some(), "{body}");

    // 3. chat session 探测。
    let session_id: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_session(workspace_id, agent_id, creator_id) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(agent_id)
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("insert chat_session");
    let (status, body) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/chat-sessions/{session_id}/gc-check"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "chat session gc-check: {body}");
    assert!(body["status"].as_str().is_some(), "{body}");

    // 4. autopilot run 探测：workspace 是从 autopilot 反查的。
    let run_id = seed_autopilot_run(&pool, workspace_id, user_id).await;
    let (status, body) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/autopilot-runs/{run_id}/gc-check"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "autopilot run gc-check: {body}");
    assert_eq!(body["status"], json!("completed"));
    assert!(body.get("completed_at").is_some(), "{body}");

    // 5. task 探测。
    let (status, body) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/tasks/{task_id}/gc-check"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "task gc-check: {body}");
    assert_eq!(body["status"], json!("queued"));

    // 未知 id ⇒ 404（且用 daemon 面的上游短语）。
    let missing = "00000000-0000-4000-8000-000000000000";
    for (path, expected) in [
        (format!("/api/daemon/issues/{missing}/gc-check"), "issue not found"),
        (
            format!("/api/daemon/chat-sessions/{missing}/gc-check"),
            "chat session not found",
        ),
        (
            format!("/api/daemon/autopilot-runs/{missing}/gc-check"),
            "autopilot run not found",
        ),
        (format!("/api/daemon/tasks/{missing}/gc-check"), "task not found"),
    ] {
        let (status, body) = support::call(&app, "GET", &path, user_id, Some("m1"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert_eq!(support::error_message(&body), expected, "{path}");
    }

    support::cleanup(&pool, workspace_id, &[user_id]).await;
    let _ = runtime_id;
}

/// 跨 workspace 不可见：别人的 workspace 的 issue 探测 ⇒ 404（不泄漏存在性）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn gc_check_hides_other_workspaces() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let (other_ws, other_user) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);
    let (_runtime_id, _agent_id, other_task) =
        support::seed_ready_task(&pool, other_ws, other_user, "m1").await;
    let other_issue: Uuid =
        sqlx::query_scalar("SELECT issue_id FROM agent_task_queue WHERE id = $1")
            .bind(other_task)
            .fetch_one(&pool)
            .await
            .expect("other issue");

    let (status, body) = support::call(
        &app,
        "GET",
        &format!("/api/daemon/issues/{other_issue}/gc-check"),
        user_id,
        Some("m1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(support::error_message(&body), "issue not found");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
    support::cleanup(&pool, other_ws, &[other_user]).await;
}

/// 批量探测的请求体大小上限（`MAX_ISSUE_GC_BODY_BYTES` = 64 KiB）之外的体 ⇒ 400。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn batch_gc_check_rejects_oversized_batch() {
    let Some((pool, db)) = support::connect().await else {
        return;
    };
    let (workspace_id, user_id) = support::seed_workspace(&pool, "owner").await;
    let app = support::app_with_db(db);

    let ids: Vec<String> = (0..600)
        .map(|i| format!("00000000-0000-4000-8000-{i:012}"))
        .collect();
    let (status, body) = support::call(
        &app,
        "POST",
        &format!("/api/daemon/workspaces/{workspace_id}/issues/gc-check"),
        user_id,
        Some("m1"),
        Some(json!({ "issue_ids": ids })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    support::cleanup(&pool, workspace_id, &[user_id]).await;
}
