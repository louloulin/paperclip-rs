//! `/api/agents/:id/env` 端到端测试（上游 `agent_env.go` L141 / L190）。
//!
//! 形状按**上游源码**核实：GET 出**明文**并先落审计行（fail-closed），`****`
//! 哨兵只作用于 PUT 输入。

use axum::http::StatusCode;
use serde_json::json;

use crate::support::{
    call, cleanup, connect, create_agent, id_of, new_agent_body, seed_runtime, seed_user,
    seed_workspace,
};

async fn env_rows(
    pool: &sqlx::PgPool,
    agent_id: uuid::Uuid,
    action: &str,
) -> Vec<serde_json::Value> {
    sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT details FROM activity_log \
         WHERE action = $1 AND details->>'agent_id' = $2 ORDER BY created_at ASC",
    )
    .bind(action)
    .bind(agent_id.to_string())
    .fetch_all(pool)
    .await
    .expect("activity_log rows")
}

/// GET 给明文、先写审计；PUT 的 `****` 哨兵保留原值、未知键丢弃。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn env_get_reveals_plaintext_with_audit_and_put_honours_sentinel() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let other = seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let runtime_id = seed_runtime(&pool, ws, Some(user), "public").await;
    let created = create_agent(&app, ws, user, new_agent_body("envious", runtime_id)).await;
    let agent_id = id_of(&created);
    let path = format!("/api/agents/{agent_id}/env");

    // 初始空
    let (status, body) = call(&app, "GET", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 唯一一次 GET → 恰好一条 `agent_env_revealed`，issue_id 恒 NULL
    let rows = env_rows(&pool, agent_id, "agent_env_revealed").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["key_count"], 0);

    // PUT 两个键
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "custom_env": { "TOKEN": "s3cret", "REGION": "eu" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["custom_env"]["TOKEN"], "s3cret");
    assert_eq!(body["agent_id"], agent_id.to_string());

    // GET 回读明文（不是 `***`/`****`），且 `has_custom_env` / key_count 在 agent 上更新
    let (status, body) = call(&app, "GET", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["custom_env"]["TOKEN"], "s3cret");
    assert_eq!(
        env_rows(&pool, agent_id, "agent_env_revealed").await.len(),
        2
    );
    let (_, agent) = call(
        &app,
        "GET",
        &format!("/api/agents/{agent_id}/"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(agent["has_custom_env"], true);
    assert_eq!(agent["custom_env_key_count"], 2);

    // PUT `****` 哨兵：TOKEN 保留原值；NEW 库里没有 → 丢弃；REGION 没提交 → 删除
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "custom_env": { "TOKEN": "****", "NEW": "****" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["custom_env"]["TOKEN"], "s3cret");
    assert!(body["custom_env"].get("NEW").is_none());
    assert!(body["custom_env"].get("REGION").is_none());

    let updated = env_rows(&pool, agent_id, "agent_env_updated").await;
    assert_eq!(updated.len(), 2);
    assert_eq!(updated[1]["preserved_keys"], json!(["TOKEN"]));
    assert_eq!(updated[1]["removed_keys"], json!(["REGION"]));

    // 非 owner 的 member：读 / 写都 403（`canManageAgentEnv`）
    let (status, body) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"]["message"].as_str().unwrap().contains("env"));
    let (status, _) = call(
        &app,
        "PUT",
        &path,
        ws,
        other,
        Some(json!({ "custom_env": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // admin 可以读（不是 owner）
    let admin = seed_user(&pool, ws, "admin").await;
    let (status, _) = call(&app, "GET", &path, ws, admin, None).await;
    assert_eq!(status, StatusCode::OK);

    // 形状不符的 body → 400（避免「解码失败 → 静默清空 env」）
    let (status, _) = call(&app, "PUT", &path, ws, user, Some(json!([1, 2]))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "custom_env": 7 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 键值不是字符串 → 400（`CustomEnv` 是 `BTreeMap<String, String>`）
    let (status, _) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "custom_env": { "A": 1 } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // env 端点同样受 agent 加载门槛保护（不存在的 id → 404）
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/agents/{}/env", uuid::Uuid::new_v4()),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, ws, &[user, other, admin]).await;
}
