//! 拒绝阶梯：上游 `squad.go:976` 的每一档拒绝，按**检查顺序**平铺。
//!
//! 顺序不是实现细节 —— 上游注释（`squad.go:1043-1051`）把它写成安全判据：会回显 task 派生
//! id 的拒绝必须排在两道门之后。本用例按顺序逐档验证；顺序本身另由 `order.rs` 专测。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    app_with_db, cleanup, connect, err_message, insert_task, path, seed, send, written_rows,
    AGENT_ID_HEADER, ONLY_LEADER_ERROR, OUTCOME_ERROR, TASK_ID_HEADER, TASK_NOT_BELONG,
};

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 阶梯按上游检查顺序平铺；拆函数就看不出「顺序」这件事
async fn rejection_ladder_matches_upstream_order() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let app = app_with_db(db);
    let fixture = seed(&pool).await;
    let leader = fixture.leader_agent.to_string();
    let other_agent = fixture.other_agent.to_string();
    let foreign_agent = fixture.foreign_agent.to_string();
    let task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        true,
        Some(fixture.squad_id),
    )
    .await;
    let task_s = task.to_string();
    let ok_body = Some(json!({"outcome": "action", "reason": "r"}));
    let good = [
        (TASK_ID_HEADER, task_s.as_str()),
        (AGENT_ID_HEADER, leader.as_str()),
    ];

    // ---- 身份 / 租户（上游 `loadIssueForUser` 那一层）----
    // 非成员 ⇒ 404 workspace（本仓 M2 约定：不泄漏 workspace 是否存在）。
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.outsider,
        &good,
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "非成员: {body}");
    assert_eq!(body["error"]["code"], json!("not_found"));

    // 不存在的 issue / 别的 workspace 的 issue ⇒ 都是 404（租户收窄）。
    for issue in [Uuid::new_v4(), fixture.foreign_issue_id] {
        let (status, body) = send(
            &app,
            fixture.workspace_id,
            fixture.member,
            &good,
            &path(issue),
            ok_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "issue={issue}: {body}");
        assert_eq!(body["error"]["code"], json!("not_found"));
    }

    // ---- 第 2、3 步：body / outcome（**先于**任何 task 检查）----
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[],
        &path(fixture.issue_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "坏 body: {body}");
    assert!(
        err_message(&body).ends_with("invalid request body"),
        "{body}"
    );

    for bad in [
        json!({}),
        json!({"outcome": ""}),
        json!({"outcome": "Action"}),
        json!({"outcome": "no action"}),
        json!({"outcome": "success"}),
    ] {
        let (status, body) = send(
            &app,
            fixture.workspace_id,
            fixture.member,
            &good,
            &path(fixture.issue_id),
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} ⇒ 400: {body}");
        assert!(
            err_message(&body).ends_with(OUTCOME_ERROR),
            "文案逐字: {body}"
        );
    }

    // ---- 第 4 步：`X-Task-ID` 是硬要求（头，不是 body 字段）----
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[(AGENT_ID_HEADER, leader.as_str())],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "缺 X-Task-ID: {body}");
    assert!(
        err_message(&body).ends_with("task id must be a uuid"),
        "{body}"
    );

    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, "not-a-uuid"),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "非 uuid 的 X-Task-ID: {body}"
    );

    // ---- 第 5 步：task 不在本 workspace / 没有 issue ----
    let foreign_task = insert_task(
        &pool,
        fixture.foreign_agent,
        fixture.foreign_runtime_id,
        Some(fixture.foreign_issue_id),
        true,
        None,
    )
    .await;
    let foreign_task_s = foreign_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, foreign_task_s.as_str()),
            (AGENT_ID_HEADER, foreign_agent.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "跨 workspace 的 task: {body}"
    );
    assert!(err_message(&body).ends_with(TASK_NOT_BELONG), "{body}");

    let chat_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        None,
        true,
        Some(fixture.squad_id),
    )
    .await;
    let chat_task_s = chat_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, chat_task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "任务没有 issue: {body}");
    assert!(err_message(&body).ends_with(TASK_NOT_BELONG), "{body}");

    // ---- 闸门 1（第 6 步）：调用者不是这条任务的 agent ----
    // (a) 完全不带 agent 头（= member）⇒ 403。
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[(TASK_ID_HEADER, task_s.as_str())],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "member 身份: {body}");
    assert!(err_message(&body).ends_with(ONLY_LEADER_ERROR), "{body}");

    // (b) agent 头存在但不是这条任务的 agent ⇒ 403（**不是** 400）。
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, task_s.as_str()),
            (AGENT_ID_HEADER, other_agent.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "别的 agent: {body}");

    // (c) agent 头非 uuid / 是别的 workspace 的 agent ⇒ 回退 member ⇒ 403。
    for bad_agent in ["not-a-uuid", foreign_agent.as_str()] {
        let (status, body) = send(
            &app,
            fixture.workspace_id,
            fixture.member,
            &[
                (TASK_ID_HEADER, task_s.as_str()),
                (AGENT_ID_HEADER, bad_agent),
            ],
            &path(fixture.issue_id),
            ok_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "agent={bad_agent}: {body}");
        assert!(err_message(&body).ends_with(ONLY_LEADER_ERROR), "{body}");
    }

    // 对照：同一对 id 是合法的（下面第 7..11 步的负例都拿它当基线）。
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &good,
        &path(fixture.issue_id),
        Some(json!({"outcome": "failed", "reason": "boom"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "对照：合法一发 {body}");

    // ---- 第 7 步：task 跑在别的 issue 上 ----
    let other_issue_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.other_issue_id),
        true,
        Some(fixture.squad_id),
    )
    .await;
    let other_issue_task_s = other_issue_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, other_issue_task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "task 在别的 issue 上: {body}"
    );
    let message = err_message(&body);
    assert!(
        message.starts_with("validation error: task does not belong to issue"),
        "{body}"
    );
    assert!(
        message.contains(&fixture.other_issue_id.to_string()),
        "**过了**闸门 1 才允许回显 task 自己的 issue id: {body}"
    );

    // ---- 第 8 步：不是 leader 任务 ----
    let worker_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        false,
        Some(fixture.squad_id),
    )
    .await;
    let worker_task_s = worker_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, worker_task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非 leader 任务: {body}");
    assert!(
        err_message(&body).ends_with("task is not a squad leader task"),
        "{body}"
    );

    // ---- 第 9 步：leader 任务但没盖 squad_id ----
    let no_squad_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        true,
        None,
    )
    .await;
    let no_squad_task_s = no_squad_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, no_squad_task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "没有 squad_id: {body}");
    assert!(
        err_message(&body).ends_with("leader task has no squad_id"),
        "{body}"
    );

    // ---- 第 10 步：squad 不存在 / 不在本 workspace ⇒ 404 ----
    let foreign_squad_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        true,
        Some(Uuid::new_v4()),
    )
    .await;
    let foreign_squad_task_s = foreign_squad_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, foreign_squad_task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "查不到的 squad: {body}");
    assert!(err_message(&body).ends_with("not found: squad"), "{body}");

    // ---- 闸门 2（第 11 步）：入队后 leader 被换掉 ----
    //
    // 这条 task 是**入队给 `leader_agent` 的 leader 任务**（`is_leader_task = true`、
    // `squad_id` 有效）⇒ 闸门 1 过；但那个 squad 现在的 `leader_id` 已经是 `other_agent`。
    // 只信行就会让这种「降级运行」写下 leader 判决（并在 `no_action` 时抑制自己的评论）
    // ⇒ 必须 403。
    let swapped_task = insert_task(
        &pool,
        fixture.leader_agent,
        fixture.runtime_id,
        Some(fixture.issue_id),
        true,
        Some(fixture.swapped_squad_id),
    )
    .await;
    let swapped_task_s = swapped_task.to_string();
    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &[
            (TASK_ID_HEADER, swapped_task_s.as_str()),
            (AGENT_ID_HEADER, leader.as_str()),
        ],
        &path(fixture.issue_id),
        ok_body,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "leader 已被轮换: {body}");
    assert!(err_message(&body).ends_with(ONLY_LEADER_ERROR), "{body}");

    // 整条阶梯上只有那一发 201 落了行。
    assert_eq!(
        written_rows(&pool, fixture.issue_id).await,
        1,
        "所有负例都没写入"
    );

    cleanup(&fixture).await;
}
