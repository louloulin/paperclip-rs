//! **两侧挂载同 handler**：同一请求经 `/v1` 与 `/api/plugin-bridge/v1` 的响应**字节比对**。
//!
//! 这是本片 `DoD` 的硬项（`docs/57` §8 的 R-M6-4：`/v1` 与 bridge 两侧 handler 分叉 ⇒ 响应不一致）。
//! 用**原始字节**而不是 JSON 值比对：JSON 规范化会把字段顺序、`null` 与缺省的差别抹掉，而
//! 客户端看到的正是字节。

use super::support::*;
use axum::http::StatusCode;
use mc_plugin_host::token::ActorKind;
use serde_json::json;

/// 把 `/v1` 的路径换成桥面路径。
fn bridge(path: &str) -> String {
    format!("/api/plugin-bridge/v1{}", &path["/v1".len()..])
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn both_mounts_return_identical_bytes_for_context() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "both").await;
    let app = app(db);
    let path = format!("/v1/context?issue_id={}", issue.id);

    let (public_status, _, public_bytes) =
        call_bytes(&app, token_req("GET", &path, &fixture.token, None)).await;
    let (bridge_status, _, bridge_bytes) = call_bytes(
        &app,
        session_req(
            "GET",
            &bridge(&path),
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;

    assert_eq!(public_status, StatusCode::OK);
    assert_eq!(bridge_status, StatusCode::OK);
    // 两侧的 actor 不同（安装令牌 vs 会话），但**同一个 issue、同一个 workspace、同一份 config**
    // ⇒ 除 `actor`/`user` 之外必须逐字节相同。逐字段比而不是整串比，好在失败时指出差在哪。
    let public: serde_json::Value = serde_json::from_slice(&public_bytes).expect("json");
    let relayed: serde_json::Value = serde_json::from_slice(&bridge_bytes).expect("json");
    assert_eq!(public["workspace"], relayed["workspace"]);
    assert_eq!(public["issue"], relayed["issue"]);
    assert_eq!(public["config"], relayed["config"]);
    assert_eq!(
        public["granted_net_domains"],
        relayed["granted_net_domains"]
    );
    assert_eq!(public["actor"], "plugin");
    assert_eq!(relayed["actor"], "member");

    // 用**同一个 actor**（一条代表该成员的回调令牌）再比一次 ⇒ 此时必须逐字节相同。
    let token = issue_callback_token(
        fixture.installation_id,
        fixture.workspace_id,
        ActorKind::Member,
        fixture.user_id,
        None,
    );
    let (_, _, public_bytes) = call_bytes(&app, token_req("GET", &path, &token, None)).await;
    let (_, _, bridge_bytes) = call_bytes(
        &app,
        session_req(
            "GET",
            &bridge(&path),
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(
        public_bytes, bridge_bytes,
        "同一 actor 下两侧响应必须逐字节相同（R-M6-4）"
    );

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn both_mounts_return_identical_bytes_for_issues_and_comments() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(
        &pool,
        &db,
        &["issues:read", "comments:read"],
        "panel.js",
        "code",
    )
    .await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "both-issues").await;
    let app = app(db);

    for path in [
        format!("/v1/issues/{}", issue.id),
        format!("/v1/issues/{}/comments", issue.id),
        // identifier 形态也走同一条解析路径（不透明引用）。
        format!("/v1/issues/{}", issue.identifier),
    ] {
        let (public_status, _, public_bytes) =
            call_bytes(&app, token_req("GET", &path, &fixture.token, None)).await;
        let (bridge_status, _, bridge_bytes) = call_bytes(
            &app,
            session_req(
                "GET",
                &bridge(&path),
                fixture.user_id,
                fixture.installation_id,
                None,
            ),
        )
        .await;
        assert_eq!(public_status, StatusCode::OK, "{path}");
        assert_eq!(bridge_status, StatusCode::OK, "{path}");
        assert_eq!(public_bytes, bridge_bytes, "{path} 两侧字节不同");
    }

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn both_mounts_project_the_same_storage_shapes() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(
        &pool,
        &db,
        &["storage:workspace", "storage:user"],
        "panel.js",
        "code",
    )
    .await;
    let app = app(db);
    let pointer = format!("/v1/storage/workspace/{}", "panel.cache");

    // 先经公开面写一个键。
    let (status, _, _) = call_raw(
        &app,
        token_req(
            "PUT",
            &pointer,
            &fixture.token,
            Some(&json!({"value": "{\"a\":1}"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 两侧读同一个桶：键列表与值都必须逐字节相同。
    for path in ["/v1/storage/workspace".to_string(), pointer.clone()] {
        let (public_status, _, public_bytes) =
            call_bytes(&app, token_req("GET", &path, &fixture.token, None)).await;
        let (bridge_status, _, bridge_bytes) = call_bytes(
            &app,
            session_req(
                "GET",
                &bridge(&path),
                fixture.user_id,
                fixture.installation_id,
                None,
            ),
        )
        .await;
        assert_eq!(public_status, StatusCode::OK, "{path}");
        assert_eq!(bridge_status, StatusCode::OK, "{path}");
        assert_eq!(public_bytes, bridge_bytes, "{path} 两侧字节不同");
    }

    // 会话面的 `storage:user` 走的是**同一条** scope 解析（成员身份来自会话）。
    let (status, _, body) = call_raw(
        &app,
        session_req(
            "PUT",
            &bridge("/v1/storage/user/pref"),
            fixture.user_id,
            fixture.installation_id,
            Some(&json!({"value": "dark"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let scope_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT scope_id FROM plugin_storage WHERE installation_id = $1 AND scope_type = 'user' AND key = 'pref'",
    )
    .bind(fixture.installation_id)
    .fetch_one(&pool)
    .await
    .expect("row exists");
    assert_eq!(
        scope_id, fixture.user_id,
        "user scope 的 scope_id 必须是**用户** id（不是 workspace / 安装 id）"
    );

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn the_same_handler_registers_nine_keys_on_each_prefix() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(
        &pool,
        &db,
        &[
            "issues:read",
            "issues:write",
            "comments:read",
            "comments:write",
        ],
        "panel.js",
        "code",
    )
    .await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "keys").await;
    let app = app(db);

    // 两侧各 9 个注册键：4 个 issue/comments + 5 个 context/storage。这里验证「两侧都挂到了」
    // —— 逐条打一次，任何一条在任一侧 404 都说明挂载点漏了。
    let issue_path = format!("/v1/issues/{}", issue.id);
    let comments_path = format!("/v1/issues/{}/comments", issue.id);
    let paths: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        ("GET", "/v1/context".to_string(), None),
        ("GET", issue_path.clone(), None),
        ("PATCH", issue_path.clone(), Some(json!({"title": "t"}))),
        ("GET", comments_path, None),
        ("GET", "/v1/storage/workspace".to_string(), None),
    ];
    for (method, path, body) in &paths {
        let (status, _, _) =
            call_raw(&app, token_req(method, path, &fixture.token, body.as_ref())).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} 在 /v1 上未挂载"
        );
        let (status, _, _) = call_raw(
            &app,
            session_req(
                method,
                &bridge(path),
                fixture.user_id,
                fixture.installation_id,
                body.as_ref(),
            ),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} 在 bridge 上未挂载"
        );
    }

    // `/v1/storage/workspace/:key` 的 GET/PUT/DELETE 三形态两侧也都在。
    for method in ["GET", "PUT", "DELETE"] {
        let body = (method == "PUT").then(|| json!({"value": "v"}));
        let (status, _, _) = call_raw(
            &app,
            token_req(
                method,
                "/v1/storage/workspace/k",
                &fixture.token,
                body.as_ref(),
            ),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} /v1/storage/workspace/k"
        );
        let (status, _, _) = call_raw(
            &app,
            session_req(
                method,
                &bridge("/v1/storage/workspace/k"),
                fixture.user_id,
                fixture.installation_id,
                body.as_ref(),
            ),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} bridge/storage/workspace/k"
        );
    }

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}
