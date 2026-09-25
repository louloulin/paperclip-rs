//! 正常路径：201 + 回读 `activity_log` 七列 + 「同一对 id 记错 issue」的 400。

use serde_json::json;
use uuid::Uuid;

use axum::http::StatusCode;

use crate::support::{
    app_with_db, cleanup, connect, err_message, insert_task, path, seed, send, written_rows,
    AGENT_ID_HEADER, TASK_ID_HEADER, TASK_NOT_BELONG,
};

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 201 + 七列回读 + 三条负例平铺；拆开就看不出是一条链
async fn records_evaluation_row_and_response() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
        return;
    };
    let app = app_with_db(db);
    let fixture = seed(&pool).await;

    let agent = fixture.leader_agent.to_string();
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
    let good = [
        (TASK_ID_HEADER, task_s.as_str()),
        (AGENT_ID_HEADER, agent.as_str()),
    ];

    let (status, body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &good,
        &path(fixture.issue_id),
        Some(json!({"outcome": "action", "reason": "closed the loop"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "正常路径 201: {body}");

    // 响应 = 三个字符串字段（上游 `map[string]string`）。
    assert!(body["id"].is_string(), "{body}");
    assert_eq!(body["action"], json!("squad_leader_evaluated"));
    assert!(
        body["created_at"].as_str().is_some_and(|s| s.contains('T')),
        "RFC3339 时间戳: {body}"
    );

    // 落库七列逐列回读。
    let activity_id = Uuid::parse_str(body["id"].as_str().unwrap()).expect("响应 id 是 uuid");
    let (ws, issue, actor_type, actor_id, action, details): (
        Uuid,
        Option<Uuid>,
        Option<String>,
        Option<Uuid>,
        String,
        serde_json::Value,
    ) = sqlx::query_as(
        "SELECT workspace_id, issue_id, actor_type, actor_id, action, details \
         FROM activity_log WHERE id = $1",
    )
    .bind(activity_id)
    .fetch_one(&pool)
    .await
    .expect("回读 activity_log");
    assert_eq!(ws, fixture.workspace_id);
    assert_eq!(issue, Some(fixture.issue_id), "记在**路径**那个 issue 上");
    assert_eq!(actor_type.as_deref(), Some("agent"));
    assert_eq!(
        actor_id,
        Some(fixture.leader_agent),
        "actor_id = task.agent_id，不是 squad.leader_id"
    );
    assert_eq!(action, "squad_leader_evaluated");
    assert_eq!(
        details,
        json!({
            "squad_id": fixture.squad_id.to_string(),
            "task_id": task_s,
            "outcome": "action",
            "reason": "closed the loop",
        })
    );

    // 第二次调用：`no_action` 再落一条**独立**的行（上游没有去重），缺省的 `reason` 是空串。
    let (second_status, second) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &good,
        &path(fixture.issue_id),
        Some(json!({"outcome": "no_action"})),
    )
    .await;
    assert_eq!(second_status, StatusCode::CREATED, "{second}");
    assert_ne!(second["id"], body["id"], "两次调用两条独立的行");
    let stored_reason: Option<String> =
        sqlx::query_scalar("SELECT details->>'reason' FROM activity_log WHERE id = $1")
            .bind(Uuid::parse_str(second["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .expect("reason");
    assert_eq!(
        stored_reason.as_deref(),
        Some(""),
        "空 reason 是空串，不是 null"
    );
    assert_eq!(written_rows(&pool, fixture.issue_id).await, 2);

    // 同一对 id 打到**另一条 issue** 上 ⇒ 400，且文案指向 task 真正跑着的那条 issue
    // （过了闸门 1 才允许回显，见 `order.rs`）。
    let (wrong_issue_status, wrong_issue_body) = send(
        &app,
        fixture.workspace_id,
        fixture.member,
        &good,
        &path(fixture.other_issue_id),
        Some(json!({"outcome": "action"})),
    )
    .await;
    assert_eq!(
        wrong_issue_status,
        StatusCode::BAD_REQUEST,
        "task 跑在 issue #1 上、记到 issue #2: {wrong_issue_body}"
    );
    let message = err_message(&wrong_issue_body);
    assert!(message.contains(TASK_NOT_BELONG), "{wrong_issue_body}");
    assert!(
        message.contains(&fixture.issue_id.to_string()),
        "文案指向 task 真正的 issue: {wrong_issue_body}"
    );
    assert_eq!(
        written_rows(&pool, fixture.issue_id).await,
        2,
        "这一发 400 没有写入"
    );

    cleanup(&fixture).await;
}
