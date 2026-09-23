//! `/api/runtimes/{id}/usage*` + `/activity` 四条读端点的端到端测试。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, call_anon, cleanup, connect, error_message, seed_agent, seed_runtime, seed_task,
    seed_user, seed_workspace, RuntimeSeed,
};

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn usage_reads_require_membership_and_usability() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let (other_workspace, outsider) = seed_workspace(&pool, "member").await;

    let own = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let uri = format!("/api/runtimes/{own}/usage");

    assert_eq!(call_anon(&app, "GET", &uri).await, StatusCode::UNAUTHORIZED);

    let (status, _) = call(&app, "GET", &uri, workspace_id, outsider, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // `private` 是别人的机器：admin 也没有读豁免（upstream MUL-6126）。
    let (status, _) = call(&app, "GET", &uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(&app, "GET", &uri, workspace_id, member, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]), "runtime 无用量时回空数组");

    // `public` 机器任何成员都能读，但**无主**机器谁都读不到（owner 才能签 task token）。
    let public = seed_runtime(
        &pool,
        workspace_id,
        Some(member),
        "public",
        RuntimeSeed::default(),
    )
    .await;
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{public}/usage"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let orphan = seed_runtime(&pool, workspace_id, None, "public", RuntimeSeed::default()).await;
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{orphan}/usage"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(
        &app,
        "GET",
        "/api/runtimes/not-a-uuid/usage",
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "runtime_id must be a uuid");

    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{}/usage", Uuid::new_v4()),
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, workspace_id, &[admin, member]).await;
    cleanup(&pool, other_workspace, &[outsider]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn usage_endpoints_project_the_seeded_rows() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = crate::support::app_with_db(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;

    let rt = seed_runtime(
        &pool,
        workspace_id,
        Some(owner),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let agent = seed_agent(&pool, workspace_id, rt, "usage-agent", None).await;
    let task = seed_task(&pool, workspace_id, owner, rt, agent, "completed").await;

    // 日历日桶（`/usage`）：provider 大小写不敏感（SQL 里 `LOWER(provider)`）。
    sqlx::query(
        "INSERT INTO task_usage_hourly \
            (bucket_hour, workspace_id, runtime_id, agent_id, provider, model, \
             input_tokens, output_tokens) \
         VALUES (now(), $1, $2, $3, 'Claude', 'opus-4', 100, 7)",
    )
    .bind(workspace_id)
    .bind(rt)
    .bind(agent)
    .execute(&pool)
    .await
    .expect("insert task_usage_hourly");

    // 任务级明细（`by-agent` / `by-hour`）。
    sqlx::query(
        "INSERT INTO task_usage (task_id, provider, model, input_tokens, output_tokens) \
         VALUES ($1, 'claude', 'opus-4', 42, 3)",
    )
    .bind(task)
    .execute(&pool)
    .await
    .expect("insert task_usage");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{rt}/usage?days=7&tz=UTC"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "usage failed: {body}");
    assert_eq!(body.as_array().map(Vec::len), Some(1));
    assert_eq!(body[0]["runtime_id"], rt.to_string());
    assert_eq!(body[0]["provider"], "claude");
    assert_eq!(body[0]["model"], "opus-4");
    assert_eq!(body[0]["input_tokens"], 100);
    assert_eq!(body[0]["output_tokens"], 7);
    assert!(
        body[0]["date"].as_str().unwrap().len() == 10,
        "date must be a calendar day: {body}"
    );

    // `days=bad` 不报错（上游同样回落到默认窗口），tz 只影响窗口边界。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{rt}/usage/by-agent?days=bad&tz=Europe/Berlin"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "by-agent failed: {body}");
    assert_eq!(body.as_array().map(Vec::len), Some(1));
    assert_eq!(body[0]["agent_id"], agent.to_string());
    assert_eq!(body[0]["provider"], "claude");
    assert_eq!(body[0]["input_tokens"], 42);
    assert_eq!(body[0]["task_count"], 1);

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{rt}/usage/by-hour?tz=UTC"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "by-hour failed: {body}");
    assert_eq!(body.as_array().map(Vec::len), Some(1));
    assert_eq!(body[0]["model"], "opus-4");
    assert_eq!(body[0]["input_tokens"], 42);
    assert!(
        body[0]["hour"]
            .as_i64()
            .is_some_and(|h| (0..24).contains(&h)),
        "hour must be an hour-of-day: {body}"
    );

    // `activity` 没有日期维度：直接数 `started_at`（不吃 `days`）。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{rt}/activity?days=1&tz=UTC"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "activity failed: {body}");
    assert_eq!(body.as_array().map(Vec::len), Some(1));
    assert_eq!(body[0]["count"], 1);

    // 另一台机器没有任何任务 → 空数组（不是 404）。
    let other = seed_runtime(
        &pool,
        workspace_id,
        Some(owner),
        "private",
        RuntimeSeed::default(),
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/runtimes/{other}/activity"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));

    cleanup(&pool, workspace_id, &[owner]).await;
}
