//! 两个信任面的**门**（M6-7）：前缀、会话头、开关、停用、未知安装。

use super::support::*;
use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn v1_requires_a_plugin_bearer_token() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app(db);
    // 完全没有 Authorization（浏览器会话不能跨进公开面）。
    let (status, headers, body) = call_raw(&app, token_req("GET", "/v1/context", "", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "plugin_bearer_required");
    assert_eq!(body["detail"], "plugin bearer token required");
    assert_eq!(body["status"], 401);
    assert_eq!(body["error"], "plugin bearer token required");
    assert!(headers.get("x-request-id").is_some());
    // 令牌家族只认 mpi_/mpc_：PAT / 乱码一律 401（上游 PluginBearerOnly）。
    for stranger in ["pat_abc", "not-a-token", "MPI_upper"] {
        let (status, _, body) =
            call_raw(&app, token_req("GET", "/v1/context", stranger, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{stranger}");
        assert_eq!(body["code"], "plugin_bearer_required", "{stranger}");
    }
    // 前缀对但查不到 ⇒ 403 `invalid plugin token`（不是 401：这是 handler 的结论）。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", "/v1/context", "mpi_does-not-exist", None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["detail"], "invalid plugin token");
    cleanup(&pool, uuid::Uuid::nil(), &[]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn bridge_requires_a_session_and_refuses_plugin_tokens() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "console.log(1)").await;
    let app = app(db);

    // 桥面拿到插件令牌 ⇒ 401 `session_required`（两个信任面互不越界）。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", "/api/plugin-bridge/v1/context", &fixture.token, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "session_required");

    // 没有会话头 ⇒ 401 `unauthorized`（上游 `pluginSessionCaller`）。
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/plugin-bridge/v1/context")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
    assert_eq!(body["detail"], "missing authenticated user");

    // 会话身份合法但安装不存在 ⇒ 404（上游 `AuthorizePluginAction` 的 `ErrNoRows` 分支）。
    let request = session_req(
        "GET",
        "/api/plugin-bridge/v1/context",
        fixture.user_id,
        uuid::Uuid::nil(),
        None,
    );
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "plugin installation not found");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn session_face_needs_an_installation_header() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let app = app(db);

    // 空安装头 ⇒ 400（上游 `plugin installation is required`）。
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/plugin-bridge/v1/context")
        .header(USER_ID_HEADER, fixture.user_id.to_string())
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "plugin installation is required");

    // 格式不对的安装 id ⇒ 404（上游把「格式不对」与「不存在」合成同一句）。
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/plugin-bridge/v1/context")
        .header(USER_ID_HEADER, fixture.user_id.to_string())
        .header(INSTALLATION_HEADER, "not-a-uuid")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "plugin installation not found");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn feature_flag_off_is_403_plugin_api_disabled_without_retry_after() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    // 显式登记为 false（本地口径：未登记 = 开启，见 routes/v1/policy.rs 文件头偏离 1）。
    let app = app_with_plugins_v1(db.clone(), false);

    // 与 `contracts/golden/context/001` 逐字对齐：`mpi_invalid` + 开关关闭 ⇒ 403 + 无 Retry-After。
    let (status, headers, body) =
        call_raw(&app, token_req("GET", "/v1/context", "mpi_invalid", None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["status"], 403);
    assert_eq!(body["code"], "plugin_api_disabled");
    assert_eq!(body["title"], "Forbidden");
    assert_eq!(body["detail"], "Plugin management is not enabled");
    assert_eq!(body["error"], "Plugin management is not enabled");
    assert!(body["type"]
        .as_str()
        .unwrap()
        .ends_with("plugin_api_disabled"));
    assert!(headers.get("retry-after").is_none(), "开关门不是可重试错误");

    // 桥面同码同体（上游 `pluginCaller` 对两侧都先过 `requirePluginActionV1`）。
    let (status, _, body) = call_raw(
        &app,
        session_req(
            "GET",
            "/api/plugin-bridge/v1/context",
            fixture.user_id,
            fixture.installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "plugin_api_disabled");

    // 未登记（= 开启）时同一个请求放行到凭据解析 ⇒ 403 `invalid plugin token`（证明开关确实是原因）。
    let (status, _, body) = call_raw(
        &app_with_plugins_v1(db, true),
        token_req("GET", "/v1/context", "mpi_invalid", None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["detail"], "invalid plugin token");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn disabled_installation_is_off_not_merely_hidden() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let (workspace_id, user_id) = seed_workspace(&pool).await;
    let manifest = panel_manifest("panel.js", &["issues:read"]);
    let (version_id, _) = seed_package(
        &pool,
        workspace_id,
        "itest-panel",
        &manifest,
        &[("panel.js", "code")],
    )
    .await;
    let (token, hash) = install_token();
    let installation_id = seed_installation(
        &pool,
        workspace_id,
        "itest-panel",
        version_id,
        &manifest,
        &["issues:read"],
        false,
        Some(&hash),
    )
    .await;
    let app = app(db);

    // 安装令牌路径：停用的插件就是关掉的（一个留在旧标签页里的 iframe 不能继续工作）。
    let (status, _, body) = call_raw(&app, token_req("GET", "/v1/context", &token, None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["detail"], "this Plugin is disabled");

    // 会话路径同判据。
    let (status, _, body) = call_raw(
        &app,
        session_req(
            "GET",
            "/api/plugin-bridge/v1/context",
            user_id,
            installation_id,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["detail"], "this Plugin is disabled");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn missing_scope_is_refused_before_the_resource_is_touched() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    // 只授 issues:read ⇒ 写面与评论面都该 403。
    let fixture = seed_panel(&pool, &db, &["issues:read"], "panel.js", "code").await;
    let issue = seed_issue(&db, fixture.workspace_id, fixture.user_id, "scoped").await;
    let app = app(db);

    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PATCH",
            &format!("/v1/issues/{}", issue.id),
            &fixture.token,
            Some(&json!({"title": "nope"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        "this Plugin was not granted the issues:write scope"
    );

    let (status, _, body) = call_raw(
        &app,
        token_req(
            "GET",
            &format!("/v1/issues/{}/comments", issue.id),
            &fixture.token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        "this Plugin was not granted the comments:read scope"
    );

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}
