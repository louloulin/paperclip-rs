//! 三个 workspace 级聚合端点的端到端测试（上游 `agent.go` L2884 / L2925 / L2976）。
//!
//! 三个端点都是**顶层 JSON 数组**，且都按 `accessibleAgentIDs` 白名单过滤。

use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::support::{
    call, cleanup, connect, create_agent, id_of, new_agent_body, seed_runtime, seed_task,
    seed_user, seed_workspace,
};

fn find<'a>(rows: &'a Value, agent_id: &str) -> Vec<&'a Value> {
    rows.as_array()
        .expect("top-level array")
        .iter()
        .filter(|row| row["agent_id"] == agent_id)
        .collect()
}

/// run-counts / activity-30d / task-snapshot 的形状 + 白名单过滤。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn workspace_aggregations_return_arrays_and_respect_visibility() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let stranger = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let agent = create_agent(&app, ws, user, new_agent_body("busy", runtime_id)).await;
    let agent_id = id_of(&agent);
    let quiet = create_agent(&app, ws, user, new_agent_body("idle", runtime_id)).await;

    // 1 条在飞 + 1 条 completed（1 小时前）+ 1 条 failed（2 天前）
    let _active = seed_task(&pool, runtime_id, agent_id, "queued", None).await;
    let _ok = seed_task(&pool, runtime_id, agent_id, "completed", Some("1 hour")).await;
    let _fail = seed_task(&pool, runtime_id, agent_id, "failed", Some("2 days")).await;

    // --- run-counts ---
    let (status, body) = call(&app, "GET", "/api/agent-run-counts", ws, user, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mine = find(&body, &agent_id.to_string());
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["run_count"], 3);
    assert_eq!(
        mine[0].as_object().unwrap().len(),
        2,
        "只有 agent_id/run_count"
    );

    // --- activity-30d ---
    let (status, body) = call(&app, "GET", "/api/agent-activity-30d", ws, user, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let buckets = find(&body, &agent_id.to_string());
    assert_eq!(buckets.len(), 2, "两条终态任务落在两个不同的天桶");
    let completed: Vec<&Value> = buckets
        .iter()
        .copied()
        .filter(|b| b["completed_count"] == 1)
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["task_count"], 1);
    assert_eq!(completed[0]["failed_count"], 0);
    let failed: Vec<&Value> = buckets
        .iter()
        .copied()
        .filter(|b| b["failed_count"] == 1)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["cancelled_count"], 0);
    // `bucket_at` 是 RFC3339 字符串
    assert!(completed[0]["bucket_at"].as_str().unwrap().contains('T'));

    // --- task-snapshot：在飞半边 + 每 agent Top-1 结果半边 ---
    let (status, body) = call(&app, "GET", "/api/agent-task-snapshot", ws, user, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mine = find(&body, &agent_id.to_string());
    assert_eq!(mine.len(), 2);
    let statuses: Vec<&str> = mine.iter().map(|t| t["status"].as_str().unwrap()).collect();
    assert!(statuses.contains(&"queued"));
    assert!(
        statuses.contains(&"completed"),
        "Top-1 取最新终态: {statuses:?}"
    );
    assert!(
        find(&body, &id_of(&quiet).to_string()).is_empty(),
        "没有任务的 agent 不进快照"
    );

    // 非 owner 的 member 看不到 private agent 的任何聚合行 → 三个端点都是 `[]`
    for path in [
        "/api/agent-run-counts",
        "/api/agent-activity-30d",
        "/api/agent-task-snapshot",
    ] {
        let (status, body) = call(&app, "GET", path, ws, stranger, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body, json!([]), "{path} 必须过滤掉不可见 agent");
    }

    // owner/admin 视角：取消在飞任务后，snapshot 的在飞半边清空
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/agents/{agent_id}/cancel-tasks"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(&app, "GET", "/api/agent-task-snapshot", ws, user, None).await;
    let mine = find(&body, &agent_id.to_string());
    assert_eq!(mine.len(), 1, "被取消的在飞行不再出现在快照里");

    cleanup(&pool, ws, &[user, stranger]).await;
}
