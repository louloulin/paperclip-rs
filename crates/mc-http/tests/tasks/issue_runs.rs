//! `POST /api/issues/preview-trigger`、`GET /api/issues/:id/active-task`、
//! `GET /api/issues/:id/task-runs`。
//!
//! preview-trigger 是 docs/15 §1.6 的「取回 source context」前置面：只做**只读判定**。
//! 这里逐条覆盖上游 `WillEnqueueRun` 的 assign / status / triage / backlog 分支。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    assert_bad_request, call, req, seed_agent, seed_issue, send, setup, TaskSeed,
};

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn preview_trigger_handles_create_and_reassign() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let agent = fx.agent_id.to_string();

    // 建单即指派 agent 且落到 todo → 会派
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "is_create": true, "assignee_type": "agent", "assignee_id": agent, "status": "todo"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["triggers"][0]["issue_id"], Uuid::nil().to_string());
    assert_eq!(body["triggers"][0]["agent_id"], agent);
    assert_eq!(body["triggers"][0]["source"], "assign");

    // 建单即落 backlog：停放区，永不派
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "is_create": true, "assignee_type": "agent", "assignee_id": agent, "status": "backlog"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0);

    // 建单指派 squad：本仓无 squad 仓储 ⇒ 不派（docs/41 §5）
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "is_create": true,
            "assignee_type": "squad",
            "assignee_id": Uuid::new_v4().to_string(),
            "status": "todo"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0);

    // 已有 issue：从未指派改派给 agent
    let plain = fx.issue("todo").await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "issue_ids": [plain.to_string()], "assignee_type": "agent", "assignee_id": agent
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["triggers"][0]["issue_id"], plain.to_string());
    assert_eq!(body["triggers"][0]["source"], "assign");

    // 预览是**只读**的：先真正把 issue 改派到该 agent，再 preview 同样的目标 ⇒ 无变化、不起火
    sqlx::query("UPDATE issue SET assignee_type = 'agent', assignee_id = $2 WHERE id = $1")
        .bind(plain)
        .bind(fx.agent_id)
        .execute(&fx.pool)
        .await
        .expect("assign issue");
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "issue_ids": [plain.to_string()], "assignee_type": "agent", "assignee_id": agent
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["total_count"], 0,
        "改派到已在位的同一个 agent 不再起火"
    );

    // triage 里的 issue 即使改派也不派
    let triaged = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        None,
        true,
        None,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "issue_ids": [triaged.to_string()], "assignee_type": "agent", "assignee_id": agent
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0, "triage 比 backlog 更严");

    // 畸形 / 未知 issue id 静默不贡献
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "issue_ids": ["nope", Uuid::new_v4().to_string()],
            "assignee_type": "agent",
            "assignee_id": agent
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0);

    // 畸形 assignee_id 是确定性 400
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({
            "is_create": true, "assignee_type": "agent", "assignee_id": "nope"
        })),
    )
    .await;
    assert_bad_request(status, &body, "invalid assignee_id");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn preview_trigger_status_source_and_issue_id_limit() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();

    // backlog → todo 的状态迁移会起火（来源是 status）
    let parked = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "backlog",
        Some(("agent", fx.agent_id)),
        false,
        None,
    )
    .await;
    let preview = |issue_ids: serde_json::Value, extra: serde_json::Value| {
        let mut body = extra;
        body["issue_ids"] = issue_ids;
        body
    };
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(preview(
            json!([parked.to_string()]),
            json!({ "status": "todo" }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["triggers"][0]["source"], "status");

    // 已有 pending 任务时，status 源不再承诺（会被 (issue, agent) 唯一索引合并）
    TaskSeed::queued(parked).insert(&fx).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(preview(
            json!([parked.to_string()]),
            json!({ "status": "todo" }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0);

    // 状态迁移到 done / cancelled 不起火
    let parked2 = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "backlog",
        Some(("agent", fx.agent_id)),
        false,
        None,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(preview(
            json!([parked2.to_string()]),
            json!({ "status": "done" }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0);

    // > 500 条 issue_ids
    let many: Vec<String> = (0..501).map(|_| Uuid::new_v4().to_string()).collect();
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "issue_ids": many })),
    )
    .await;
    assert_bad_request(status, &body, "too many issue_ids");

    // 空 body 也能解析（全部字段有默认）
    let (status, body) = call(
        &app,
        "POST",
        "/api/issues/preview-trigger",
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total_count"], 0);

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn active_task_and_task_runs_scope_issue() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let other_agent = seed_agent(&fx.pool, fx.workspace_id, fx.runtime_id, Some(fx.user_id)).await;

    let queued = TaskSeed::queued(fx.issue_id).insert(&fx).await;
    let running = TaskSeed::queued(fx.issue_id)
        .agent(other_agent)
        .status("running")
        .insert(&fx)
        .await;
    let done = TaskSeed::queued(fx.issue_id)
        .agent(other_agent)
        .status("completed")
        .insert(&fx)
        .await;
    // 未启动的升级占位行：不进执行日志（上游 visibleTaskHistory）
    let escalation = TaskSeed::queued(fx.issue_id)
        .status("deferred")
        .not_started()
        .escalation(queued)
        .insert(&fx)
        .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{}/active-task", fx.issue_id),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<String> = body["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|row| row["id"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(ids.len(), 2, "{body}");
    assert!(ids.contains(&queued.to_string()) && ids.contains(&running.to_string()));
    assert!(!ids.contains(&done.to_string()), "completed 不在在飞集合里");
    assert!(body["tasks"][0]["status"].is_string(), "{body}");

    // 全量执行日志
    let uri = format!("/api/issues/{}/task-runs", fx.issue_id);
    let (status, body) = call(&app, "GET", &uri, fx.workspace_id, fx.user_id, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let all: Vec<String> = body
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row["id"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(all.len(), 3, "{body}");
    assert!(all.contains(&done.to_string()));
    assert!(!all.contains(&escalation.to_string()), "升级占位行不外泄");

    // active=true 只留在飞
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?active=true"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 2);
    // 显式 active=false 等价于默认
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?active=false"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 3);

    // 参数校验
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?active=maybe"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "invalid active parameter; expected boolean");
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?scope=team"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "scope must be 'issue' or 'family'");

    // 未知 issue
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{}/task-runs", Uuid::new_v4()),
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
async fn task_runs_family_truncates_at_cap() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let parent = fx.issue("todo").await;

    let mut children = Vec::new();
    for _ in 0..22 {
        let child = seed_issue(
            &fx.pool,
            fx.workspace_id,
            fx.user_id,
            "todo",
            Some(("agent", fx.agent_id)),
            false,
            Some(parent),
        )
        .await;
        TaskSeed::queued(child).insert(&fx).await;
        children.push(child);
    }
    // 父 issue 自己也有一个在飞任务（族根包含自己）
    TaskSeed::queued(parent).insert(&fx).await;

    let (status, headers, body) = send(
        &app,
        req(
            "GET",
            &format!("/api/issues/{parent}/task-runs?scope=family"),
            fx.workspace_id,
            fx.user_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers
            .get("x-active-runs-truncated")
            .map(|v| v.to_str().unwrap()),
        Some("true"),
        "22 个子 issue + 1 个父 + 1 个族根任务 > cap(20) ⇒ 必须给出截断信号"
    );
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 20);
    assert_eq!(rows[0]["status"], "queued");
    assert!(
        rows[0]["issue_identifier"]
            .as_str()
            .expect("identifier")
            .starts_with("IT-"),
        "{body}"
    );
    assert!(
        rows[0]["issue_title"]
            .as_str()
            .expect("title")
            .starts_with("itest-issue-"),
        "{body}"
    );
    assert!(rows[0]["task_id"].as_str().is_some() && rows[0]["agent_id"].as_str().is_some());

    // 子 issue 用 scope=family 看到同一个族根（族根 = 父，不是自己）
    let (status, headers, body) = send(
        &app,
        req(
            "GET",
            &format!("/api/issues/{}/task-runs?scope=family", children[0]),
            fx.workspace_id,
            fx.user_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 20);
    assert_eq!(
        headers
            .get("x-active-runs-truncated")
            .map(|v| v.to_str().unwrap()),
        Some("true")
    );

    // scope=issue 仍然走全量分支（不受族预算影响）
    let (status, headers, body) = send(
        &app,
        req(
            "GET",
            &format!("/api/issues/{parent}/task-runs?scope=issue"),
            fx.workspace_id,
            fx.user_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 1);
    assert!(headers.get("x-active-runs-truncated").is_none());

    fx.cleanup().await;
}
