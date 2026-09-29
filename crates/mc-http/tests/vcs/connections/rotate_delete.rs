//! `rotate-webhook` / `DELETE` 面。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 =
//! `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。
//! **纯移动**：断言与夹具调用逐字未改。

use super::*;

// ---------------------------------------------------------------------------
// rotate / delete
// ---------------------------------------------------------------------------

/// `rotate-webhook`：旧 secret **立刻失效**、新 secret **立刻生效**、明文**只此一次**。
///
/// 三条判据都在真实 webhook 帧上验证（HMAC 用哪把 secret 算 —— 见 `tests/vcs/webhook.rs`
/// 的 `forgejo_frame`）。这里只断言响应与库。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rotate_returns_one_time_secret_and_nothing_else_does() {
    let fx = fixture!(ready);
    let instance = serve_stub(forgejo_stub("acme-bot", StatusCode::OK)).await;
    let (status, created, _) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.connections_uri(),
            Some(fx.admin),
            &json!({"provider": "forgejo", "instance_url": instance, "access_token": "fj-pat"})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let connection_id = Uuid::parse_str(created["id"].as_str().expect("id")).expect("uuid");
    let first_secret = created["webhook_secret"]
        .as_str()
        .expect("secret")
        .to_string();

    // rotate：响应里带新明文，且与旧的不同。
    let (status, rotated, _) = send(
        &fx.app,
        req_json("POST", &fx.rotate_uri(connection_id), Some(fx.admin), ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let second_secret = rotated["webhook_secret"]
        .as_str()
        .expect("secret")
        .to_string();
    assert_ne!(first_secret, second_secret);
    assert_eq!(rotated["id"], created["id"]);

    // 库里只有**新** secret（旧 secret 立刻失效 = 它不再存在）。
    let (_, _, _, secret_enc) = fx.raw_connection(fx.ws).await.expect("row");
    let opened = open(&secret_enc);
    assert_eq!(opened, second_secret, "轮换后库里是新 secret");
    assert_ne!(opened, first_secret);

    // rotate 之后再读列表：**任何**读面都不再返回 secret（只此一次）。
    let (status, _, list_raw) = call(&fx.app, "GET", &fx.connections_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!list_raw.contains(&second_secret));
    assert!(!list_raw.contains(&first_secret));

    // 跨 workspace 的 connectionId ⇒ 404（与不存在同判）。
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let (status, body, _) = send(
        &fx.app,
        req_json(
            "POST",
            &format!("/api/workspaces/{other_ws}/vcs/connections/{connection_id}/rotate-webhook"),
            Some(fx.admin),
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    cleanup(&fx.pool, other_ws, &[other_admin]).await;

    fx.teardown().await;
}

/// `DELETE`：级联清掉镜像 PR / 关联账 / CI 状态（这 4 张表没有 FK，靠一条 CTE 原子完成）；
/// 重复删仍回 204（上游 `:exec` 不看 `rows_affected`）；跨 workspace 删是 no-op。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn delete_cascades_child_rows_and_is_idempotent() {
    let fx = fixture!(ready);
    let connection_id = crate::support::seed_connection(
        &fx.pool,
        fx.ws,
        "forgejo",
        "https://git.test",
        &crate::support::seal("seed-secret"),
    )
    .await;
    let (pr_id, _issue_id) = fx.seed_child_rows(connection_id).await;

    // 跨 workspace 删 → no-op（204，行还在）。
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &format!("/api/workspaces/{other_ws}/vcs/connections/{connection_id}"),
        Some(other_admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        fx.raw_connection(fx.ws).await.is_some(),
        "跨租户删必须 no-op"
    );
    cleanup(&fx.pool, other_ws, &[other_admin]).await;

    // 本 workspace 删 → 204 + 三张子表都清空。
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.delete_uri(connection_id),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(fx.raw_connection(fx.ws).await.is_none());
    for (table, column) in [
        ("vcs_pull_request", "workspace_id"),
        ("vcs_commit_status", "connection_id"),
    ] {
        let sql = format!("SELECT count(*) FROM {table} WHERE {column} = $1");
        let count: i64 = sqlx::query_scalar(&sql)
            .bind(if table == "vcs_pull_request" {
                fx.ws
            } else {
                connection_id
            })
            .fetch_one(&fx.pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} 未级联清空");
    }
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issue_vcs_pull_request WHERE pull_request_id = $1",
    )
    .bind(pr_id)
    .fetch_one(&fx.pool)
    .await
    .expect("count links");
    assert_eq!(links, 0, "issue_vcs_pull_request 未级联清空");

    // 幂等：再删一次仍然 204。
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.delete_uri(connection_id),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    fx.teardown().await;
}
