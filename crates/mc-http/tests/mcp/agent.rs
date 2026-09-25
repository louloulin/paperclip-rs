//! agent MCP 绑定 4 条路由的端到端测试（M8-3 / `LUM-1800`）。
//!
//! 专属 `DoD` 的两条在这里兑现（`docs/61` §6.5 的 M8-3 行）：
//! ① **`enabled` 开关幂等**（连开两次不插两行、绑定存活）；
//! ② **授权面是 `loadAgentForUser` 语义，不是裸 workspace member**（member / guest 403，
//!    agent owner 与 workspace admin 放行，非本 workspace 的 agent 404）。
//! 外加：跨租户 `server_id` ⇒ 404 且**一行都不落**；响应仍然 write-only。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    agent_enabled_uri, agent_item_uri, agent_uri, binding_enabled, call_scoped, count_bindings,
    req_json, seed_server, seed_user, seed_workspace, send, skipped, Fx,
};

/// 上游 `workspaceMcpTestSecret` 逐字。
const SECRET: &str = "sk-live-workspace-should-never-be-echoed";
const ENTRY: &str = r#"{"url":"https://linear.example","headers":{"Authorization":"Bearer sk-live-workspace-should-never-be-echoed"}}"#;

/// agent 面发请求（自动带上 workspace 头）。
async fn agent_call(
    fx: &Fx,
    method: &str,
    uri: &str,
    who: Option<Uuid>,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value, String) {
    call_scoped(&fx.app, method, uri, who, Some(fx.ws), body).await
}

// ---------------------------------------------------------------------------
// 绑定生命周期
// ---------------------------------------------------------------------------

/// 上游 `TestAgentMcpServerBinding_AddToggleRemove` 的移植 + 幂等断言（落库行数）。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn binding_add_toggle_remove_is_idempotent() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let agent = fx.seed_agent(fx.admin).await;
    let server = fx.seed_server("linear", ENTRY).await;
    let add_body = json!({"server_id": server.to_string()}).to_string();

    // 加：响应就是**更新后的绑定列表**，客户端不必猜。
    let (status, list, raw) = agent_call(
        &fx,
        "POST",
        &agent_uri(agent),
        Some(fx.admin),
        Some(&add_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    let entries = list.as_array().expect("array").clone();
    assert_eq!(entries.len(), 1, "{list}");
    assert_eq!(entries[0]["id"], server.to_string());
    assert_eq!(entries[0]["enabled"], true, "新加的绑定必须是开的");
    // write-only：绑定列表里也不得出现条目的值。
    assert!(!raw.contains(SECRET), "{raw}");
    assert!(!raw.contains("linear.example"), "{raw}");

    // 再加一次：**幂等**（不是错误，也不重复插行）。
    let (status, list, raw) = agent_call(
        &fx,
        "POST",
        &agent_uri(agent),
        Some(fx.admin),
        Some(&add_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(
        list.as_array().expect("array").len(),
        1,
        "重复 add 复制了绑定"
    );
    assert_eq!(count_bindings(&fx.pool, server).await, 1);

    // 关：绑定**存活**（关掉再打开不必重新找一遍）。
    let (status, list, raw) = agent_call(
        &fx,
        "PUT",
        &agent_enabled_uri(agent, server),
        Some(fx.admin),
        Some("{\"enabled\":false}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(list.as_array().expect("array").len(), 1);
    assert_eq!(list[0]["enabled"], false);
    assert_eq!(binding_enabled(&fx.pool, agent, server).await, Some(false));

    // 再关一次（同一个值）：仍然**幂等** —— 命中同一行，不插新行。
    let (status, _, raw) = agent_call(
        &fx,
        "PUT",
        &agent_enabled_uri(agent, server),
        Some(fx.admin),
        Some("{\"enabled\":false}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(count_bindings(&fx.pool, server).await, 1);

    // 再开：同一个绑定回到 true（没被删过）。
    let (status, list, raw) = agent_call(
        &fx,
        "PUT",
        &agent_enabled_uri(agent, server),
        Some(fx.admin),
        Some("{\"enabled\":true}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(list[0]["enabled"], true);
    assert_eq!(binding_enabled(&fx.pool, agent, server).await, Some(true));

    // 摘：列表空，**库条目本身不动**。
    let (status, list, raw) = agent_call(
        &fx,
        "DELETE",
        &agent_item_uri(agent, server),
        Some(fx.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(list.as_array().expect("array").len(), 0);
    let library_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspace_mcp_server WHERE id = $1")
            .bind(server)
            .fetch_one(&fx.pool)
            .await
            .expect("count library rows");
    assert_eq!(library_rows, 1, "摘绑定把库条目一起删了");

    // 摘两次：第二次 404（绑定已经不在了）。
    let (status, _, _) = agent_call(
        &fx,
        "DELETE",
        &agent_item_uri(agent, server),
        Some(fx.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    fx.teardown().await;
}

/// 改名之后绑定**跟着走**（绑定以 id 为键，不是以名字为键）—— 这正是 314 的文档模型被换掉的
/// 理由。顺带覆盖「加过的条目改名后列表里的新名字」。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn rename_keeps_the_binding() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let agent = fx.seed_agent(fx.admin).await;
    let server = fx.seed_server("linear", ENTRY).await;
    let add_body = json!({"server_id": server.to_string()}).to_string();
    let (status, _, _) = agent_call(
        &fx,
        "POST",
        &agent_uri(agent),
        Some(fx.admin),
        Some(&add_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, raw) = send(
        &fx.app,
        req_json(
            "PUT",
            &fx.library_item_uri(server),
            Some(fx.admin),
            &json!({"name":"linear-v2"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (status, list, _) = agent_call(&fx, "GET", &agent_uri(agent), Some(fx.admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["name"], "linear-v2", "绑定没跟着改名");

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 授权面
// ---------------------------------------------------------------------------

/// 上游 `requireAgentMcpWriter`：`loadAgentForUser` + `canViewAgentSecrets`
/// （agent owner 或 workspace owner/admin），**不是**裸 workspace member。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn binding_authorization_is_load_agent_for_user() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    // agent 的 owner 是 `member`（普通成员，不是 admin）—— 这正是要区分的两个身份。
    let agent = fx.seed_agent(fx.member).await;
    let server = fx.seed_server("linear", ENTRY).await;
    let add_body = json!({"server_id": server.to_string()}).to_string();

    let uri = agent_uri(agent);
    let enabled_uri = agent_enabled_uri(agent, server);
    let item_uri = agent_item_uri(agent, server);

    // **agent owner**（member 角色）四条全放行。
    let (status, _, _) = agent_call(&fx, "GET", &uri, Some(fx.member), None).await;
    assert_eq!(status, StatusCode::OK, "agent owner 的读");
    let (status, _, _) = agent_call(&fx, "POST", &uri, Some(fx.member), Some(&add_body)).await;
    assert_eq!(status, StatusCode::OK, "agent owner 的加");
    let (status, _, _) = agent_call(
        &fx,
        "PUT",
        &enabled_uri,
        Some(fx.member),
        Some("{\"enabled\":false}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "agent owner 的开关");
    let (status, _, _) = agent_call(&fx, "DELETE", &item_uri, Some(fx.member), None).await;
    assert_eq!(status, StatusCode::OK, "agent owner 的摘");

    // **admin** 放行（即使不是 owner）。
    let (status, _, _) = agent_call(&fx, "GET", &uri, Some(fx.admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = agent_call(&fx, "POST", &uri, Some(fx.admin), Some(&add_body)).await;
    assert_eq!(status, StatusCode::OK);

    // **裸 workspace member**（既非 owner 也非 admin）⇒ 403（四条都要）。
    let (status, _, _) = agent_call(&fx, "GET", &uri, Some(fx.guest), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "guest 的读");
    let (status, _, _) = agent_call(&fx, "POST", &uri, Some(fx.guest), Some(&add_body)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = agent_call(
        &fx,
        "PUT",
        &enabled_uri,
        Some(fx.guest),
        Some("{\"enabled\":false}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = agent_call(&fx, "DELETE", &item_uri, Some(fx.guest), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 另一个**同 workspace 的普通成员**也不行（不是「非成员才拒」）。
    let second = seed_user(&fx.pool, fx.ws, "member").await;
    let (status, _, _) = agent_call(&fx, "GET", &uri, Some(second), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "别的 member 也不该读得到");

    // 缺 workspace ⇒ 400；非成员 ⇒ 404；无会话 ⇒ 401。
    let (status, _, _) = call_scoped(&fx.app, "GET", &uri, Some(fx.admin), None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "缺 workspace");
    let (status, _, _) = agent_call(&fx, "GET", &uri, Some(fx.outsider), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "非成员");
    let (status, _, _) = call_scoped(&fx.app, "GET", &uri, None, Some(fx.ws), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "没有会话头");

    // 不存在的 agent / 别的 workspace 的 agent / 非 `kind='user'` 的 agent ⇒ 404。
    let (status, _, _) =
        agent_call(&fx, "GET", &agent_uri(Uuid::new_v4()), Some(fx.admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "不存在的 agent");
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let foreign = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO agent(workspace_id, name, runtime_mode, kind, owner_id) \
         VALUES ($1, 'foreign-agent', 'local', 'user', $2) RETURNING id",
    )
    .bind(other_ws)
    .bind(other_admin)
    .fetch_one(&fx.pool)
    .await
    .expect("insert foreign agent");
    let (status, _, _) = agent_call(&fx, "GET", &agent_uri(foreign), Some(fx.admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "别的 workspace 的 agent");
    let system_agent = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO agent(workspace_id, name, runtime_mode, kind, owner_id) \
         VALUES ($1, 'system-agent', 'local', 'system', $2) RETURNING id",
    )
    .bind(fx.ws)
    .bind(fx.admin)
    .fetch_one(&fx.pool)
    .await
    .expect("insert system agent");
    let (status, _, _) =
        agent_call(&fx, "GET", &agent_uri(system_agent), Some(fx.admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "kind<>user 的 agent");
    let (status, _, _) = agent_call(
        &fx,
        "GET",
        "/api/agents/not-a-uuid/mcp-servers",
        Some(fx.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "坏 agent id（上游同判 404）");

    crate::support::cleanup(&fx.pool, other_ws, &[other_admin]).await;
    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 边界：跨租户 / 缺绑定 / 坏 body
// ---------------------------------------------------------------------------

/// 跨租户的 `server_id` **不得**跨 workspace 绑定（上游 404，且一行都不落库）。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn binding_rejects_a_server_from_another_workspace() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let agent = fx.seed_agent(fx.admin).await;
    let (other_ws, other_admin) = seed_workspace(&fx.pool, "admin").await;
    let foreign = seed_server(&fx.pool, other_ws, "foreign", ENTRY).await;

    let (status, _, raw) = agent_call(
        &fx,
        "POST",
        &agent_uri(agent),
        Some(fx.admin),
        Some(&json!({"server_id": foreign.to_string()}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
    assert_eq!(
        count_bindings(&fx.pool, foreign).await,
        0,
        "跨租户绑定落库了"
    );

    crate::support::cleanup(&fx.pool, other_ws, &[other_admin]).await;
    fx.teardown().await;
}

/// 缺绑定 / 坏 body / 缺 `enabled` 的边界(状态码逐条对齐上游)。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn binding_error_cases() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let agent = fx.seed_agent(fx.admin).await;
    let unbound = fx.seed_server("never-bound", ENTRY).await;

    // 没绑定过 ⇒ 开关 / 摘都 404。
    let (status, _, _) = agent_call(
        &fx,
        "PUT",
        &agent_enabled_uri(agent, unbound),
        Some(fx.admin),
        Some("{\"enabled\":false}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = agent_call(
        &fx,
        "DELETE",
        &agent_item_uri(agent, unbound),
        Some(fx.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // `enabled` 缺字段 / `null` / 非布尔 ⇒ 400。
    for raw in ["{}", "null", "{\"enabled\":null}", "{\"enabled\":\"true\"}"] {
        let (status, _, _) = agent_call(
            &fx,
            "PUT",
            &agent_enabled_uri(agent, unbound),
            Some(fx.admin),
            Some(raw),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body={raw}");
    }

    // `server_id` 缺失 / 非法 uuid ⇒ 400；body 非对象 ⇒ 400。
    for raw in ["{}", "null", "{\"server_id\":\"not-a-uuid\"}"] {
        let (status, _, _) =
            agent_call(&fx, "POST", &agent_uri(agent), Some(fx.admin), Some(raw)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body={raw}");
    }
    for raw in ["", "[]", "not json"] {
        let (status, _, _) =
            agent_call(&fx, "POST", &agent_uri(agent), Some(fx.admin), Some(raw)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body={raw:?}");
    }

    // 坏 serverId 路径参数 ⇒ 400（早于绑定查询）。
    let (status, _, _) = agent_call(
        &fx,
        "DELETE",
        &format!("/api/agents/{agent}/mcp-servers/nope"),
        Some(fx.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 应用层栅栏（两张表都没有 FK ⇒ 竞态只能靠自己防）
// ---------------------------------------------------------------------------

/// 上游 `TestAgentMcpServerAdd_CannotLandAfterServerDeleteCommits` 的移植：
/// 绑定写入**必须**停在 server 行的 `FOR SHARE` 上（`WorkspaceMcpServerRepo::delete` 的
/// `FOR UPDATE` 与它互斥），所以「删条目已扫过绑定、尚未提交」的窗口里插不进新绑定。
///
/// 这一条驱动的是**真的两个并发事务**（不是断言顺序执行的结果）：先持锁、再让真 handler 跑，
/// 并确认它确实**停在锁上**（`pg_stat_activity` 的 `wait_event_type='Lock'`），而不是「只是慢」。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn binding_add_cannot_land_after_the_server_delete_commits() {
    use std::time::Duration;

    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let agent = fx.seed_agent(fx.admin).await;
    let server = fx.seed_server("raced-server", ENTRY).await;

    // deleter：`DeleteWorkspaceMcpServer` 的第一步（独占锁住 server 行）。
    let mut deleter = fx.pool.begin().await.expect("begin deleter");
    sqlx::query("SELECT id FROM workspace_mcp_server WHERE id = $1 FOR UPDATE")
        .bind(server)
        .execute(&mut *deleter)
        .await
        .expect("lock server row");

    // 并发的绑定写入：走**真 handler**（不是直接调 repo），这样断言的是路由层的完整路径。
    let app = fx.app.clone();
    let uri = agent_uri(agent);
    let (ws, who) = (fx.ws, fx.admin);
    let body = json!({"server_id": server.to_string()}).to_string();
    let writer = tokio::spawn(async move {
        call_scoped(&app, "POST", &uri, Some(who), Some(ws), Some(&body))
            .await
            .0
    });

    // 等它真的停在锁上（轮询 `pg_stat_activity`；10s 上限只防挂死）。
    let blocked = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' \
                   AND query ILIKE '%workspace_mcp_server%FOR SHARE%'",
            )
            .fetch_one(&fx.pool)
            .await
            .expect("pg_stat_activity");
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(blocked.is_ok(), "绑定写入没有停在 server 行的 FOR SHARE 上");

    // 走完删除协议：先扫绑定、再删行、最后提交。
    sqlx::query("DELETE FROM agent_mcp_server WHERE server_id = $1")
        .bind(server)
        .execute(&mut *deleter)
        .await
        .expect("sweep bindings");
    sqlx::query("DELETE FROM workspace_mcp_server WHERE id = $1")
        .bind(server)
        .execute(&mut *deleter)
        .await
        .expect("delete server");
    deleter.commit().await.expect("commit delete");

    let status = tokio::time::timeout(Duration::from_secs(10), writer)
        .await
        .expect("绑定写入没有在删除提交后返回")
        .expect("join writer");
    assert_ne!(
        status,
        StatusCode::OK,
        "排队的绑定落在删除提交之后（栅栏没守住）"
    );
    assert_eq!(
        count_bindings(&fx.pool, server).await,
        0,
        "删除留下了孤儿绑定"
    );

    fx.teardown().await;
}
