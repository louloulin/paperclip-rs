//! workspace MCP 服务器库 4 条路由的端到端测试（M8-3 / `LUM-1800`）。
//!
//! 专属 `DoD` 的三条在这里兑现（`docs/61` §6.5 的 M8-3 行）：
//! ① **write-only**：响应**原始 JSON 字节**里 `headers` / `env` 的**值**一个字节都不出现
//!    （连 URL 也不出现 —— 它本身就是凭据材料；上游用例在同一个地方断言）；
//! ② **重名拒绝**：迁移 `316` 的唯一约束 ⇒ 409；
//! ③ **建库条目不给任何人**：创建后该 workspace 的 agent 一个都看不到它。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{agent_uri, call, call_scoped, count_bindings, req_json, send, skipped, Fx};

/// 上游 `workspaceMcpTestSecret` 逐字（用例之间靠它认「回显」）。
const SECRET: &str = "sk-live-workspace-should-never-be-echoed";
/// 上游 `workspaceMcpTestEntry` 逐字形状（URL 里也带同一枚凭证）。
const ENTRY: &str = r#"{"url":"https://linear.example","headers":{"Authorization":"Bearer sk-live-workspace-should-never-be-echoed"}}"#;

/// 建一个库条目的 body。
fn create_body(name: &str, config: &serde_json::Value) -> String {
    json!({ "name": name, "config": config }).to_string()
}

// ---------------------------------------------------------------------------
// CRUD + write-only
// ---------------------------------------------------------------------------

/// 库面 4 条的完整回路：建（201）→ 列表（member 可见、无 `enabled`、无凭据）→ 改名（200）
/// → 只用 config 更新（200）→ 删（204）→ 列表空。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn library_crud_round_trip_never_echoes_the_entry() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };

    // ① 建：**admin**，201，响应里不得有凭据 / URL。
    let (status, created, raw) = send(
        &fx.app,
        req_json("POST", &fx.library_uri(), Some(fx.admin), &create_body("linear", &json!({"url":"https://linear.example","headers":{"Authorization":format!("Bearer {SECRET}")}}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{raw}");
    assert!(!raw.contains(SECRET), "create 回显了共享凭证: {raw}");
    assert!(
        !raw.contains("linear.example"),
        "create 回显了条目 URL: {raw}"
    );
    assert_eq!(created["name"], "linear");
    assert_eq!(created["transport"], "http");
    assert_eq!(created["workspace_id"], fx.ws.to_string());
    // 库面**没有**绑定 ⇒ 不带 `enabled`。
    assert!(created.get("enabled").is_none(), "{created}");
    let server_id = created["id"].as_str().expect("id").to_string();

    // ② 列表：**member** 可见（最有权的那次读都不该拿到凭据）。
    for who in [fx.admin, fx.member, fx.guest] {
        let (status, list, raw) = call(&fx.app, "GET", &fx.library_uri(), Some(who)).await;
        assert_eq!(status, StatusCode::OK, "{raw}");
        assert!(!raw.contains(SECRET), "列表回显了共享凭证: {raw}");
        assert!(!raw.contains("linear.example"), "列表回显了条目 URL: {raw}");
        let entries = list.as_array().expect("array").clone();
        assert_eq!(entries.len(), 1, "{list}");
        assert_eq!(entries[0]["transport"], "http");
        assert!(entries[0].get("enabled").is_none(), "库列表不带 enabled");
    }

    // ③ 改名（只给 name）：条目 id 不变、transport 不变。
    let (status, renamed, raw) = send(
        &fx.app,
        req_json(
            "PUT",
            &fx.library_item_uri(Uuid::parse_str(&server_id).expect("uuid")),
            Some(fx.admin),
            &json!({"name":"linear-v2"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(renamed["name"], "linear-v2");
    assert_eq!(renamed["id"], server_id);

    // ④ 只给 config：name 保持。
    let (status, replaced, raw) = send(
        &fx.app,
        req_json(
            "PUT",
            &fx.library_item_uri(Uuid::parse_str(&server_id).expect("uuid")),
            Some(fx.admin),
            &json!({"config":{"type":"stdio","command":"npx"}}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(replaced["name"], "linear-v2", "只给 config 时不动 name");
    assert_eq!(replaced["transport"], "stdio");

    // ⑤ 删：204，列表空。
    let (status, body, _) = call(
        &fx.app,
        "DELETE",
        &fx.library_item_uri(Uuid::parse_str(&server_id).expect("uuid")),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, list, _) = call(&fx.app, "GET", &fx.library_uri(), Some(fx.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().expect("array").len(), 0);

    fx.teardown().await;
}

/// **建库条目不给任何人** + **删条目扫绑定**：两张表没有 FK，残留的绑定会指向消失的 server。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn library_entry_binds_to_nobody_and_delete_sweeps_bindings() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let agent = fx.seed_agent(fx.admin).await;

    let (status, created, raw) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.library_uri(),
            Some(fx.admin),
            &create_body("linear", &json!({"url":"https://linear.example"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{raw}");
    let server_id = Uuid::parse_str(created["id"].as_str().expect("id")).expect("uuid");

    // 建完 → 没有任何 agent 看得到它（agent 面的 workspace 走头，见 `call_scoped`）。
    assert_eq!(count_bindings(&fx.pool, server_id).await, 0);
    let (status, list, _) = call_scoped(
        &fx.app,
        "GET",
        &agent_uri(agent),
        Some(fx.admin),
        Some(fx.ws),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list.as_array().expect("array").len(),
        0,
        "没被给到就该是空的"
    );

    // 现在显式加给这个 agent。
    let (status, _, raw) = call_scoped(
        &fx.app,
        "POST",
        &agent_uri(agent),
        Some(fx.admin),
        Some(fx.ws),
        Some(&json!({"server_id": server_id.to_string()}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(count_bindings(&fx.pool, server_id).await, 1);

    // 删条目 → 绑定被同一事务扫掉。
    let (status, _, body) = call(
        &fx.app,
        "DELETE",
        &fx.library_item_uri(server_id),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        count_bindings(&fx.pool, server_id).await,
        0,
        "删除留下了孤儿绑定"
    );

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 校验反例 / 重名
// ---------------------------------------------------------------------------

/// 上游 `TestCreateWorkspaceMcpServer_RejectsBadInput` 的逐条移植（外加 transport 反例的
/// 「原样透传」一侧：非法 transport **不**是 400 —— 上游刻意让 transport 是自由字符串）。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn library_rejects_bad_input_but_keeps_unknown_transports() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };

    let bad = [
        json!({"name":"","config":{"url":"https://mcp.example"}}),
        json!({"name":"has space","config":{"url":"https://mcp.example"}}),
        json!({"name":"dot.name","config":{"url":"https://mcp.example"}}),
        json!({"name":"ok","config":{}}),
        json!({"name":"ok","config":"not-an-object"}),
        json!({"name":"ok","config":null}),
        json!({"name":"ok"}),
    ];
    for body in bad {
        let (status, _, raw) = send(
            &fx.app,
            req_json("POST", &fx.library_uri(), Some(fx.admin), &body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} ⇒ {raw}");
        // 校验错误**不得**回显条目内容（条目常规地嵌着 token）。
        assert!(!raw.contains(SECRET), "{raw}");
    }

    // 非法 transport 原样存下、原样报出（**不是** 400，也不是被洗成 http）。
    let (status, created, raw) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.library_uri(),
            Some(fx.admin),
            &create_body(
                "websockety",
                &json!({"type":"websocket","url":"wss://mcp.example"}),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{raw}");
    assert_eq!(
        created["transport"], "websocket",
        "未知 transport 必须原样透传"
    );

    fx.teardown().await;
}

/// **重名拒绝**（迁移 `316` 的唯一索引 ⇒ 409）：建与改两条路径各一条。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn library_rejects_duplicate_names() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let first = fx.seed_server("linear", ENTRY).await;
    let second = fx.seed_server("other", ENTRY).await;

    let (status, _, raw) = send(
        &fx.app,
        req_json(
            "POST",
            &fx.library_uri(),
            Some(fx.admin),
            &create_body("linear", &json!({"url":"https://other.example"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{raw}");

    // 改名撞到已存在的名字 ⇒ 同一个 409（同一条唯一约束）。
    let (status, _, raw) = send(
        &fx.app,
        req_json(
            "PUT",
            &fx.library_item_uri(second),
            Some(fx.admin),
            &json!({"name":"linear"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{raw}");

    // 原地改自己（名字不变）不是冲突。
    let (status, updated, raw) = send(
        &fx.app,
        req_json(
            "PUT",
            &fx.library_item_uri(first),
            Some(fx.admin),
            &json!({"name":"linear","config":{"type":"http","url":"https://linear.example/v2"}})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(updated["name"], "linear");

    fx.teardown().await;
}

// ---------------------------------------------------------------------------
// 授权矩阵 / 边界
// ---------------------------------------------------------------------------

/// 上游的路由分组：读在 **member** 组（含 guest）、写在 **admin** 组；非成员 404、无会话 401、
/// 坏 uuid 400、不存在的条目 404。
#[tokio::test]
#[ignore = "需要真库：MULTICA_TEST_DATABASE_URL"]
async fn library_authorization_matrix_per_endpoint() {
    let Some(fx) = Fx::new().await else {
        return skipped();
    };
    let server_id = fx.seed_server("linear", ENTRY).await;
    let body = create_body("created-by-the-test", &json!({"url":"https://mcp.example"}));

    // 读：三个成员都看得到（member 组）。
    for who in [fx.admin, fx.member, fx.guest] {
        let (status, _, _) = call(&fx.app, "GET", &fx.library_uri(), Some(who)).await;
        assert_eq!(status, StatusCode::OK);
    }
    // 写：member / guest 一律 403（admin 组）。
    for who in [fx.member, fx.guest] {
        let (status, _, _) = send(
            &fx.app,
            req_json("POST", &fx.library_uri(), Some(who), &body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "非 admin 的 create");
        let (status, _, _) = send(
            &fx.app,
            req_json(
                "PUT",
                &fx.library_item_uri(server_id),
                Some(who),
                "{\"name\":\"x\"}",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "非 admin 的 update");
        let (status, _, _) = call(
            &fx.app,
            "DELETE",
            &fx.library_item_uri(server_id),
            Some(who),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "非 admin 的 delete");
    }

    // 非成员：workspace 不可见 ⇒ 404（不是 403）。
    let (status, _, _) = call(&fx.app, "GET", &fx.library_uri(), Some(fx.outsider)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 没有会话 ⇒ 401。
    let (status, _, _) = call(&fx.app, "GET", &fx.library_uri(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // workspace id 不是 uuid ⇒ 400（早于角色判定）。
    let (status, _, _) = call(
        &fx.app,
        "GET",
        "/api/workspaces/not-a-uuid/mcp-servers",
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // server id 不是 uuid ⇒ 400；不存在的 server ⇒ 404。
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &format!("/api/workspaces/{}/mcp-servers/nope", fx.ws),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(
        &fx.app,
        "DELETE",
        &fx.library_item_uri(Uuid::new_v4()),
        Some(fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &fx.app,
        req_json(
            "PUT",
            &fx.library_item_uri(Uuid::new_v4()),
            Some(fx.admin),
            "{\"name\":\"x\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 空 body / 非对象 body ⇒ 400（上游 `json.Decoder` 的两条）。
    for raw in ["", "[]", "not json"] {
        let (status, _, _) = send(
            &fx.app,
            req_json("POST", &fx.library_uri(), Some(fx.admin), raw),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body={raw:?}");
    }
    // `null` body ⇒ 零值 ⇒ 名字缺失 ⇒ 400（**不是** 500）。
    let (status, _, _) = send(
        &fx.app,
        req_json("POST", &fx.library_uri(), Some(fx.admin), "null"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    fx.teardown().await;
}
