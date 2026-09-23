//! `POST /api/issues/:id/rerun`、`POST /api/tasks/:taskId/retry-source-context`。
//!
//! 两条路由都是「先过 invoke 门再动数据」（fail-closed），且被拒时回上游
//! `dispatchBlockedResponse` 的**原始体**（`reason_code` 不在标准信封里）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    assert_bad_request, call, error_message, req, seed_agent, seed_issue, seed_user, send, setup,
    TaskSeed,
};

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rerun_issue_derived_and_named_sources() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let uri = format!("/api/issues/{}/rerun", fx.issue_id);

    // 派生 rerun：空体 ⇒ 把当前 assignee 再跑一遍
    let (status, _, body) = send(&app, req("POST", &uri, fx.workspace_id, fx.user_id, None)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    assert_eq!(body["kind"], "direct");
    assert_eq!(body["agent_id"], fx.agent_id.to_string());
    assert_eq!(body["issue_id"], fx.issue_id.to_string());
    assert_eq!(body["workspace_id"], fx.workspace_id.to_string());
    assert!(
        body["rerun_of_task_id"].is_null(),
        "派生 rerun 没有来源任务"
    );
    let created = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    let row = sqlx::query_as::<_, (bool, String, Option<Uuid>)>(
        "SELECT force_fresh_session, originator_source, rerun_of_task_id \
         FROM agent_task_queue WHERE id = $1",
    )
    .bind(created)
    .fetch_one(&fx.pool)
    .await
    .expect("rerun row");
    assert!(row.0, "rerun 必须强制开新会话");
    assert_eq!(row.1, "direct_human");
    assert_eq!(row.2, None);

    // 具名 rerun：重复一次历史运行
    let done = TaskSeed::queued(fx.issue_id)
        .status("completed")
        .insert(&fx)
        .await;
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "task_id": done.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["rerun_of_task_id"], done.to_string());
    assert_eq!(body["status"], "queued");

    // `{}` 与缺失 task_id 都是派生语义
    let (status, body) = call(
        &app,
        "POST",
        &uri,
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(body["rerun_of_task_id"].is_null());

    // issue 也可以用 identifier 寻址
    let identifier: String = sqlx::query_scalar("SELECT identifier FROM issue WHERE id = $1")
        .bind(fx.issue_id)
        .fetch_one(&fx.pool)
        .await
        .expect("identifier");
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{identifier}/rerun"),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)]
async fn rerun_issue_guards_assignee_and_source_task() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let rerun = |issue: Uuid| format!("/api/issues/{issue}/rerun");

    // triage 里的派生 rerun：无执行者 ⇒ 403 原始体
    let triaged = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        Some(("agent", fx.agent_id)),
        true,
        None,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(triaged),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["reason_code"], "issue_in_triage");
    assert!(
        body["error"].is_string(),
        "403 是原始体，不套错误信封: {body}"
    );

    // 未指派 / 指派给人
    let plain = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        Some(("member", fx.user_id)),
        false,
        None,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(plain),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_bad_request(status, &body, "issue is not assigned to an agent or squad");

    // 指派给 squad：本仓无 squad 仓储
    let squadded = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        Some(("squad", Uuid::new_v4())),
        false,
        None,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(squadded),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_bad_request(
        status,
        &body,
        "issue is assigned to a squad but squad not found",
    );

    // 别人的私有 agent：invoke 门拒绝 ⇒ 403 原始体（旧任务不得被悄悄取消）
    let other = seed_user(&fx.pool, fx.workspace_id, "member").await;
    let other_agent = seed_agent(&fx.pool, fx.workspace_id, fx.runtime_id, Some(other)).await;
    let theirs = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        Some(("agent", other_agent)),
        false,
        None,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(theirs),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["reason_code"], "invocation_not_allowed");

    // 具名来源必须属于同一个 issue
    let elsewhere = fx.issue("todo").await;
    let stray = TaskSeed::queued(elsewhere).insert(&fx).await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(fx.issue_id),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "task_id": stray.to_string() })),
    )
    .await;
    assert_bad_request(status, &body, "source task does not belong to this issue");

    // 具名来源是 triage 运行：不能「重跑」，只能重新 triage
    let triage_task = TaskSeed::queued(fx.issue_id)
        .context(json!({ "type": "triage" }))
        .status("completed")
        .insert(&fx)
        .await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(fx.issue_id),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "task_id": triage_task.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(error_message(&body).contains("triage run"), "{body}");

    // 坏 task_id / 不存在的 task / 不存在的 issue
    let (status, body) = call(
        &app,
        "POST",
        &rerun(fx.issue_id),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "task_id": "nope" })),
    )
    .await;
    assert_bad_request(status, &body, "invalid task_id");
    let (status, body) = call(
        &app,
        "POST",
        &rerun(fx.issue_id),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "task_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_bad_request(status, &body, "load source task: task not found");
    let (status, body) = call(
        &app,
        "POST",
        &rerun(Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // triage 里的**具名** rerun 合法（重复一次讨论）：triage 门只挡派生 rerun
    let triage_done = TaskSeed::queued(triaged)
        .status("completed")
        .insert(&fx)
        .await;
    let (status, body) = call(
        &app,
        "POST",
        &rerun(triaged),
        fx.workspace_id,
        fx.user_id,
        Some(json!({ "task_id": triage_done.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["rerun_of_task_id"], triage_done.to_string());

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 断言按上游字段逐条平铺，拆函数反而更难读
async fn retry_source_context_only_for_the_original_requester() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();
    let retry = |task: Uuid| format!("/api/tasks/{task}/retry-source-context");

    let context = json!({
        "type": "quick_create",
        "source_context_id": Uuid::new_v4().to_string(),
        "requester_id": fx.user_id.to_string(),
    });
    let failed = TaskSeed {
        issue_id: None,
        status: Some("failed".to_owned()),
        context: Some(context.clone()),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;

    let (status, body) = call(
        &app,
        "POST",
        &retry(failed),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    assert_eq!(body["kind"], "quick_create");
    assert_eq!(body["rerun_of_task_id"], failed.to_string());
    assert!(body["issue_id"].is_null(), "quick-create 重试不绑 issue");
    let child = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    let row = sqlx::query_as::<_, (bool, String, Option<Uuid>)>(
        "SELECT force_fresh_session, originator_source, rerun_of_task_id \
         FROM agent_task_queue WHERE id = $1",
    )
    .bind(child)
    .fetch_one(&fx.pool)
    .await
    .expect("child row");
    assert!(row.0 && row.1 == "direct_human" && row.2 == Some(failed));

    // 源任务还没失败
    let alive = TaskSeed {
        issue_id: None,
        status: Some("queued".to_owned()),
        context: Some(context.clone()),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &retry(alive),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "source_context_retry_unavailable");
    assert!(body["error"].is_string(), "409 也是原始体: {body}");

    // 上下文类型不对 / 请求者不是调用者 / requester 缺失
    for context in [
        json!({ "type": "issue", "source_context_id": Uuid::new_v4().to_string(), "requester_id": fx.user_id.to_string() }),
        json!({ "type": "quick_create", "source_context_id": Uuid::new_v4().to_string(), "requester_id": Uuid::new_v4().to_string() }),
        json!({ "type": "quick_create", "source_context_id": Uuid::new_v4().to_string() }),
        json!({ "type": "quick_create", "requester_id": fx.user_id.to_string() }),
        json!({ "type": "quick_create", "source_context_id": "nope", "requester_id": fx.user_id.to_string() }),
    ] {
        let task = TaskSeed {
            issue_id: None,
            status: Some("failed".to_owned()),
            context: Some(context),
            ..TaskSeed::default()
        }
        .insert(&fx)
        .await;
        let (status, body) = call(
            &app,
            "POST",
            &retry(task),
            fx.workspace_id,
            fx.user_id,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "source_context_retry_unavailable", "{body}");
    }

    // 别的成员即使拿到 task id 也重放不了别人的 prompt
    let other = seed_user(&fx.pool, fx.workspace_id, "member").await;
    let (status, body) = call(&app, "POST", &retry(failed), fx.workspace_id, other, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // 不存在的 task 也是 409（不泄露存在性），坏 uuid 才是 400
    let (status, body) = call(
        &app,
        "POST",
        &retry(Uuid::new_v4()),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = call(
        &app,
        "POST",
        "/api/tasks/nope/retry-source-context",
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await;
    assert_bad_request(status, &body, "invalid task id");

    fx.cleanup().await;
}
