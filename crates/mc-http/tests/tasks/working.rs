//! `GET /api/working-agents`。

use axum::http::StatusCode;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::support::{
    assert_bad_request, call, req_with, seed_agent, seed_chat_session, seed_issue, seed_user, send,
    setup, TaskSeed,
};

/// `GET /api/working-agents` + query（两个测试共用）。
async fn get(fx: &crate::support::Fixture, query: &str) -> (StatusCode, Value) {
    call(
        &fx.app(),
        "GET",
        &format!("/api/working-agents{query}"),
        fx.workspace_id,
        fx.user_id,
        None,
    )
    .await
}

/// 期望的 issue id 集合（`ARRAY_AGG(DISTINCT …)` 按 uuid 排序，比较前先归一）。
fn ids(values: &[Uuid]) -> Vec<String> {
    let mut out: Vec<String> = values.iter().map(ToString::to_string).collect();
    out.sort_unstable();
    out
}

/// 参数校验的**顺序**也是契约（上游逐个 `switch` 先命中先回）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn working_agents_validates_parameters_in_upstream_order() {
    let Some(fx) = setup().await else { return };
    let app = fx.app();

    let (status, body) = get(&fx, "?type=bogus").await;
    assert_bad_request(
        status,
        &body,
        "invalid type: must be issue, autopilot, or chat",
    );
    // `type` 先于 `scope`：即使 scope 也非法，先报 type
    let (status, body) = get(&fx, "?type=bogus&scope=team").await;
    assert_bad_request(
        status,
        &body,
        "invalid type: must be issue, autopilot, or chat",
    );
    let (status, body) = get(&fx, "?relation=assigned").await;
    assert_bad_request(status, &body, "relation requires scope=mine");
    let (status, body) = get(&fx, "?scope=mine").await;
    assert_bad_request(status, &body, "scope=mine requires type=issue");
    let (status, body) = get(&fx, "?scope=mine&type=chat").await;
    assert_bad_request(status, &body, "scope=mine requires type=issue");
    let (status, body) = get(&fx, "?scope=team&type=issue").await;
    assert_bad_request(status, &body, "invalid scope: must be mine");
    let (status, body) = get(&fx, "?scope=mine&type=issue&relation=bogus").await;
    assert_bad_request(
        status,
        &body,
        "invalid relation: must be assigned, created, involved, or any",
    );
    let (status, body) = get(&fx, "?parent=not-a-uuid&type=issue").await;
    assert_bad_request(status, &body, "invalid parent");
    let (status, body) = get(&fx, "?parent=not-a-uuid&type=chat").await;
    assert_bad_request(status, &body, "parent requires type=issue");
    let (status, body) = get(&fx, "?scope=mine&type=issue&parent=not-a-uuid").await;
    assert_bad_request(status, &body, "parent cannot be combined with scope");
    // 空值等价于没有这个参数（上游 TrimSpace 后判空）
    let (status, body) = get(&fx, "?type=&scope=&relation=&parent=").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 没有 running 任务 ⇒ 空数组
    assert_eq!(body, json!([]));

    // 认证：缺 header / 未知用户 / 非成员 workspace
    let (status, _, body) = send(
        &app,
        req_with(
            "GET",
            "/api/working-agents",
            None,
            Some(fx.workspace_id),
            &[],
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, body) = call(
        &app,
        "GET",
        "/api/working-agents",
        fx.workspace_id,
        Uuid::new_v4(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = call(
        &app,
        "GET",
        "/api/working-agents",
        Uuid::new_v4(),
        fx.user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    fx.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn working_agents_filters_work_type_relation_and_parent() {
    let Some(fx) = setup().await else { return };

    // fx.issue：creator = user，assignee = 用户的 agent（involved / created 都命中）
    let mine_issue = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        Some(("member", fx.user_id)),
        false,
        None,
    )
    .await;
    let parent = seed_issue(
        &fx.pool,
        fx.workspace_id,
        fx.user_id,
        "todo",
        Some(("agent", fx.agent_id)),
        false,
        None,
    )
    .await;
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
    // 别人的私有 agent（不可见，SQL 取回后必须被后置过滤丢掉）
    let other = seed_user(&fx.pool, fx.workspace_id, "member").await;
    let hidden_agent = seed_agent(&fx.pool, fx.workspace_id, fx.runtime_id, Some(other)).await;

    // fx.agent 在四个 issue 上各一条 running，外加一条 running chat 任务
    let session = seed_chat_session(&fx.pool, fx.workspace_id, fx.agent_id, fx.user_id).await;
    for issue in [fx.issue_id, mine_issue, parent, child] {
        let _ = TaskSeed::queued(issue).status("running").insert(&fx).await;
    }
    let _ = TaskSeed::queued(mine_issue).insert(&fx).await;
    let _ = TaskSeed {
        issue_id: None,
        status: Some("running".to_owned()),
        chat_session_id: Some(session),
        ..TaskSeed::default()
    }
    .insert(&fx)
    .await;
    // completed / 不可见 agent 的行都不该出现
    let _ = TaskSeed::queued(fx.issue_id)
        .status("completed")
        .insert(&fx)
        .await;
    let _ = TaskSeed::queued(fx.issue_id)
        .status("running")
        .agent(hidden_agent)
        .insert(&fx)
        .await;

    // 只可见 fixture 的 agent：行数 + running_task_count + issue_ids
    let only = |body: &Value| -> Value {
        let rows = body.as_array().expect("array").clone();
        assert_eq!(rows.len(), 1, "只有 fixture 的 agent 可见: {body}");
        assert!(rows[0]["name"].as_str().is_some_and(|v| !v.is_empty()));
        rows[0].clone()
    };

    let (status, body) = get(&fx, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = only(&body);
    assert_eq!(row["running_task_count"], 5, "chat 任务也算在飞: {body}");
    assert_eq!(row["issue_ids"].as_array().map(Vec::len), Some(4));
    assert!(
        row.get("avatar_url").is_none(),
        "没有头像就不发这个键: {row}"
    );

    // type 三态：issue / chat 有行，autopilot 为空
    let (status, body) = get(&fx, "?type=issue").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(only(&body)["running_task_count"], 4);
    let (status, body) = get(&fx, "?type=chat").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(only(&body)["running_task_count"], 1);
    assert_eq!(only(&body)["issue_ids"], json!([]));
    let (status, body) = get(&fx, "?type=autopilot").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!([]));

    // relation 四态在「谁参与了这个 issue」上互相区分
    let (status, body) = get(&fx, "?type=issue&scope=mine&relation=assigned").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(only(&body)["issue_ids"], json!(ids(&[mine_issue])));
    let (status, body) = get(&fx, "?type=issue&scope=mine&relation=created").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        only(&body)["issue_ids"],
        json!(ids(&[fx.issue_id, mine_issue, parent, child])),
        "四个 issue 都是我建的"
    );
    let (status, body) = get(&fx, "?type=issue&scope=mine&relation=involved").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        only(&body)["issue_ids"],
        json!(ids(&[fx.issue_id, parent, child])),
        "指派给我的 agent 的 issue"
    );
    let (status, body) = get(&fx, "?type=issue&scope=mine&relation=any").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        only(&body)["issue_ids"],
        json!(ids(&[fx.issue_id, mine_issue, parent, child])),
    );

    // parent 收窄到直接子 issue
    let (status, body) = get(&fx, &format!("?type=issue&parent={parent}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(only(&body)["issue_ids"], json!(ids(&[child])));
    // 没有子 issue 的 issue ⇒ 空
    let (status, body) = get(&fx, &format!("?type=issue&parent={child}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!([]));

    fx.cleanup().await;
}
