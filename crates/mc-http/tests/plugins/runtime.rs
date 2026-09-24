//! `/api/workspaces/{id}/plugins*` 端到端测试：运行时面（M6-6 / LUM-1671）**调用记录 + MCP 采纳**。
//!
//! 覆盖本片 4 条注册键里的前 3 条：
//!
//! | 路由 | 本文件的用例 |
//! |---|---|
//! | `GET …/{installationId}/invocations` | 分页 / 空页 / 越界 offset / 跨安装隔离 / 门 |
//! | `GET …/{installationId}/mcp/{hookKey}/tools` | 未知 hook、http 传输、无 `net:` scope、出网失败 |
//! | `PUT …/{installationId}/mcp/{hookKey}/tools` | 请求体、同一批前置、写入未被踩到 |
//!
//! 第 4 条（surface launch）在 `runtime_surface.rs`；共用夹具在 `runtime_support.rs`。
//! 拆成三个文件是因为门 ⑩ 的单文件 800 行硬上限。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`），由门 ⑥ 用 `-- --ignored` 拉起。
//!
//! ⚠️ **出网那条用例**（`mcp/*/tools` 的 502）打的是一台**不存在**的 MCP 服务器：`mcp.example.com`
//! 解析不到、即使解析到也连不上，两种都落到同一条 `plugin_unavailable`。真要起一台 MCP 服务器
//! 得用裸 TCP 上的 HTTP/1.1 夹具，而那份夹具在 `mc-mcp` 的 `pub(crate) mod tests` 里 —— 跨 crate
//! 复用要先把它公开，代价大于收益：本片要证明的是「路由真的走到了发现那一步、并把出网失败折成
//! 502」，不是重新验一遍 MCP 协议（那是 `mc-mcp` 自己的用例）。
//!
//! ⚠️ 相反，**纯判定**（钉定取哪个摘要、漂移标记、`page_params` 的钳位、origin 解析与专用判定、
//! 令牌的 TTL / 过期 / 篡改 / 错域）在 `src/routes/plugins/{mcp,surface_launch}.rs` 的 `mod tests`
//! 里逐条断言 —— 那些不需要库，也不该等到门 ⑥ 才被验。

use super::runtime_support::*;
use super::support::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use sqlx::types::Json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ① GET …/invocations
// ---------------------------------------------------------------------------

/// `DoD`：**分页边界**（空页 / 越界 offset）+ 「最新优先」+ 跨安装隔离。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 150 行：分页的每一种边界都要先造出它自己的行集合，拆函数只会把夹具与断言分开
async fn invocations_paginate_newest_first_with_empty_and_out_of_range_pages() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;

    let inv_manifest = with_net_scope(manifest(
        "com.example.inv",
        "1.0.0",
        &mcp_hook_manifest("toolbox"),
    ));
    let installation = install_with_net(&app, workspace_id, owner, &inv_manifest, &[]).await;
    let installation_id = id_of(&installation);
    let version_id = version_uuid(&installation);

    // 另一个安装的行绝不能出现（跨安装泄露调用历史 = 泄露别的租户的行为）。
    let other_manifest = with_net_scope(manifest(
        "com.example.other",
        "1.0.0",
        &http_hook_manifest("sync"),
    ));
    let other = install_with_net(&app, workspace_id, owner, &other_manifest, &[]).await;
    let other_id = id_of(&other);
    let other_version = version_uuid(&other);

    // 第三个安装：没有任何调用行 —— 「空页」要用它，不能用 `other`（后者故意有行，验隔离）。
    let empty_manifest = with_net_scope(manifest(
        "com.example.empty",
        "1.0.0",
        &http_hook_manifest("sync"),
    ));
    let empty = install_with_net(&app, workspace_id, owner, &empty_manifest, &[]).await;
    let empty_id = id_of(&empty);
    let empty_version = version_uuid(&empty);

    let oldest = seed_invocation(
        &pool,
        workspace_id,
        installation_id,
        "sync",
        "2024-01-01T00:00:00Z",
    )
    .await;
    let middle = seed_invocation(
        &pool,
        workspace_id,
        installation_id,
        "toolbox",
        "2024-01-02T00:00:00Z",
    )
    .await;
    let newest = seed_invocation(
        &pool,
        workspace_id,
        installation_id,
        "toolbox",
        "2024-01-03T00:00:00Z",
    )
    .await;
    seed_invocation(
        &pool,
        workspace_id,
        other_id,
        "sync",
        "2024-01-04T00:00:00Z",
    )
    .await;

    let uri = plugins_uri(workspace_id, &format!("/{installation_id}/invocations"));
    let (status, body) = call(&app, "GET", &uri, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["invocations"].as_array().expect("invocations array");
    assert_eq!(items.len(), 3, "{body}");
    assert_eq!(items[0]["id"], json!(newest.to_string()));
    assert_eq!(items[1]["id"], json!(middle.to_string()));
    assert_eq!(items[2]["id"], json!(oldest.to_string()));
    // 上游 `pluginInvocationResponse` 的字段名与形态（`omitempty` 的键不该出现）。
    assert_eq!(items[0]["hook_key"], json!("toolbox"));
    assert_eq!(items[0]["trigger"], json!("manual"));
    assert_eq!(items[0]["status"], json!("failed"));
    assert_eq!(items[0]["attempt"], json!(2));
    assert_eq!(items[0]["latency_ms"], json!(41));
    assert_eq!(items[0]["created_at"], json!("2024-01-03T00:00:00Z"));
    for absent in [
        "event_type",
        "error",
        "delivery_id",
        "planned_at",
        "installation_id",
        "workspace_id",
    ] {
        assert!(
            items[0].get(absent).is_none(),
            "{absent} must not be sent: {body}"
        );
    }

    // `offset` 真的透传到 SQL —— 否则「越界返空」可以靠「offset 被忽略」蒙对。
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?limit=1&offset=1"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["invocations"][0]["id"], json!(middle.to_string()));
    let (status, body) = call(
        &app,
        "GET",
        &format!("{uri}?limit=1"),
        workspace_id,
        owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["invocations"][0]["id"], json!(newest.to_string()));

    // **越界 offset** ⇒ 空页（不是 404，也不是 `null`）。
    for past_the_end in ["limit=10&offset=3", "offset=999"] {
        let (status, body) = call(
            &app,
            "GET",
            &format!("{uri}?{past_the_end}"),
            workspace_id,
            owner,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{past_the_end}: {body}");
        assert!(
            body["invocations"].as_array().expect("array").is_empty(),
            "{past_the_end}: {body}"
        );
    }

    // **空页**：没有任何调用的安装回空表（不是 404、不是 `null`）。
    // 这里必须用第三个安装 —— `other` 那一行是**故意**插的，用来验跨安装隔离。
    let empty_uri = plugins_uri(workspace_id, &format!("/{empty_id}/invocations"));
    let (status, body) = call(&app, "GET", &empty_uri, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["invocations"].as_array().expect("array").is_empty(),
        "{body}"
    );

    // 上限钳到 500；拼错的参数回缺省而不是 400（分页是能力，不是拒服务的理由）。
    for query in ["limit=1000000", "limit=0", "limit=abc&offset=xyz"] {
        let (status, body) = call(
            &app,
            "GET",
            &format!("{uri}?{query}"),
            workspace_id,
            owner,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}: {body}");
    }

    cleanup_runtime(&pool, installation_id, version_id).await;
    cleanup_runtime(&pool, other_id, other_version).await;
    cleanup_runtime(&pool, empty_id, empty_version).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// 4 条路由的门：非法 workspace、非成员、成员但非管理员、未知安装、开关关闭。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 127 行：五类门 × 四条路由的矩阵，逐格平铺才看得出覆盖面
async fn runtime_routes_enforce_the_same_guards_as_the_rest_of_the_plugin_face() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let member = seed_user(&pool, workspace_id, "member").await;
    let (_other_workspace, outsider) = seed_workspace(&pool, "owner").await;

    let manifest = with_net_scope(manifest(
        "com.example.guard",
        "1.0.0",
        &mcp_hook_manifest("toolbox"),
    ));
    let installation = install_with_net(&app, workspace_id, owner, &manifest, &[]).await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);

    let invocations = plugins_uri(workspace_id, &format!("/{installation_id}/invocations"));
    let tools = plugins_uri(
        workspace_id,
        &format!("/{installation_id}/mcp/toolbox/tools"),
    );
    let launch = launch_uri(workspace_id, &installation_id, "panel");
    let empty_body = Some(json!({ "tools": [] }));

    // 非法 workspace id ⇒ 400 `validation_error`（四条路由逐条；判据在收请求体之前）。
    for (method, suffix) in [
        ("GET", format!("/{installation_id}/invocations")),
        ("GET", format!("/{installation_id}/mcp/toolbox/tools")),
        ("PUT", format!("/{installation_id}/mcp/toolbox/tools")),
        ("GET", format!("/{installation_id}/surfaces/panel/launch")),
    ] {
        let uri = format!("/api/workspaces/not-a-uuid/plugins{suffix}");
        let (status, body) =
            call(&app, method, &uri, workspace_id, owner, empty_body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {body}");
        assert_eq!(
            error_code(&body),
            "validation_error",
            "{method} {uri}: {body}"
        );
        assert!(
            error_message(&body).contains("workspace_id must be a valid uuid"),
            "{method} {uri}: {body}"
        );
    }

    // 非成员 ⇒ 404 `workspace`（不泄露「这个 workspace 存在」）。
    for (method, uri) in [
        ("GET", &invocations),
        ("GET", &tools),
        ("PUT", &tools),
        ("GET", &launch),
    ] {
        let (status, body) = call(
            &app,
            method,
            uri,
            workspace_id,
            outsider,
            empty_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
        assert_eq!(error_code(&body), "not_found", "{method} {uri}: {body}");
        assert_eq!(error_message(&body), "workspace", "{method} {uri}: {body}");
    }

    // 成员但非管理员：三条管理员路由 ⇒ 403；surface launch 是**成员可见**的 ⇒ 不是 403
    // （它落在 503「未配置」，因为测试 app 没配 `MULTICA_PLUGIN_SURFACE_ORIGIN`）。
    for uri in [&invocations, &tools] {
        let (status, body) = call(&app, "GET", uri, workspace_id, member, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "GET {uri}: {body}");
        assert_eq!(error_code(&body), "forbidden", "GET {uri}: {body}");
    }
    let (status, body) = call(
        &app,
        "PUT",
        &tools,
        workspace_id,
        member,
        empty_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "PUT {tools}: {body}");
    let (status, body) = call(&app, "GET", &launch, workspace_id, member, None).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "member launch: {body}"
    );
    assert_eq!(
        error_code(&body),
        "plugin_surfaces_not_configured",
        "{body}"
    );

    // 未知安装 ⇒ 404「plugin installation not found」（不是「路由不存在」的那种 404）。
    let unknown = Uuid::new_v4().to_string();
    for uri in [
        plugins_uri(workspace_id, &format!("/{unknown}/invocations")),
        plugins_uri(workspace_id, &format!("/{unknown}/mcp/toolbox/tools")),
    ] {
        let (status, body) = call(&app, "GET", &uri, workspace_id, owner, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        assert_eq!(error_code(&body), "not_found", "{uri}: {body}");
        assert_eq!(
            error_message(&body),
            "plugin installation not found",
            "{body}"
        );
    }

    // `plugins_v1` 显式关闭 ⇒ 四条都 403 `plugin_api_disabled`（上游 `requirePluginsV1`）。
    let disabled = app_with_plugins_v1_disabled(db_of(&pool));
    for (method, uri) in [
        ("GET", &invocations),
        ("GET", &tools),
        ("PUT", &tools),
        ("GET", &launch),
    ] {
        let (status, body) = call(
            &disabled,
            method,
            uri,
            workspace_id,
            owner,
            empty_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(
            error_code(&body),
            "plugin_api_disabled",
            "{method} {uri}: {body}"
        );
    }

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner, member]).await;
}

// ---------------------------------------------------------------------------
// ② GET|PUT …/mcp/{hookKey}/tools
// ---------------------------------------------------------------------------

/// 未知 hook ⇒ 404；`http` 传输的 hook 拿来当 mcp ⇒ 400。两条都**不碰网络**，所以它们同时
/// 证明「路由挂载对了、三层门过了、走到了 hook 解析」。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn mcp_tools_route_rejects_unknown_hooks_and_http_transports() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.hooks",
        "1.0.0",
        &http_hook_manifest("sync"),
    ));
    let installation = install_with_net(&app, workspace_id, owner, &manifest, &[]).await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);

    let unknown = plugins_uri(workspace_id, &format!("/{installation_id}/mcp/nope/tools"));
    for method in ["GET", "PUT"] {
        let (status, body) = call(
            &app,
            method,
            &unknown,
            workspace_id,
            owner,
            Some(json!({ "tools": [] })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}: {body}");
        assert_eq!(error_code(&body), "not_found", "{method}: {body}");
        assert_eq!(
            error_message(&body),
            "this Plugin has no hook named \"nope\"",
            "{method}: {body}"
        );
    }

    // `http` 传输的 hook 不能当 mcp 用（上游 `hook %q is not an mcp transport` ⇒ 400）。
    let http_hook = plugins_uri(workspace_id, &format!("/{installation_id}/mcp/sync/tools"));
    let (status, body) = call(&app, "GET", &http_hook, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "validation_error", "{body}");
    assert_eq!(
        error_message(&body),
        "hook \"sync\" is not an mcp transport",
        "{body}"
    );

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// `net:` scope 缺失 ⇒ 403 **且不出网**（判据在白名单取空的那一刻，早于拨号）。
/// 顺带证明「发现不采纳任何东西」：被拒的这次没有把 `mcp_approvals` 写下一行。
///
/// ⚠️ 这一行是**制造出来的坏数据**：装得干净的插件必然带着它的 `net:` scope
/// （manifest 校验器要求 hook 的 URL 被某个 `net:` scope 精确覆盖，而 `requireExactScopes`
/// 又要求授权与声明逐项相同）—— 上游的注释也这么说。所以这里装完之后直接改库，
/// 验的是那条「为被损坏 / 从旧库恢复的行而留」的第二层判据。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn mcp_discovery_needs_a_net_scope_and_adopts_nothing() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.nonet",
        "1.0.0",
        &mcp_hook_manifest("toolbox"),
    ));
    let installation = install_with_net(&app, workspace_id, owner, &manifest, &[]).await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);

    sqlx::query(
        "UPDATE plugin_installation SET granted_scopes = '[\"issues:read\"]'::jsonb WHERE id = $1",
    )
    .bind(id_of(&installation))
    .execute(&pool)
    .await
    .expect("corrupt the granted scopes");

    let tools = plugins_uri(
        workspace_id,
        &format!("/{installation_id}/mcp/toolbox/tools"),
    );
    let expected = "this Plugin was granted no net: scope, so it cannot reach an MCP server";
    let (status, body) = call(&app, "GET", &tools, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "forbidden", "{body}");
    assert_eq!(error_message(&body), expected, "{body}");

    let (status, body) = call(
        &app,
        "PUT",
        &tools,
        workspace_id,
        owner,
        Some(json!({ "tools": ["search"] })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_message(&body), expected, "{body}");

    // 被拒的两次都没有写进 `mcp_approvals`。
    let stored: Json<Value> =
        sqlx::query_scalar("SELECT mcp_approvals FROM plugin_installation WHERE id = $1")
            .bind(id_of(&installation))
            .fetch_one(&pool)
            .await
            .expect("read approvals");
    assert_eq!(stored.0, json!({}), "{:?}", stored.0);

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// 请求体形状：空体 / `tools` 不是字符串数组 ⇒ 400（上游 `json.NewDecoder` 的同一句文案），
/// 且判在**出网之前** —— 所以这两条也不依赖网络。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn approve_mcp_tools_rejects_a_bad_body_before_reaching_out() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.badbody",
        "1.0.0",
        &mcp_hook_manifest("toolbox"),
    ));
    let installation = install_with_net(&app, workspace_id, owner, &manifest, &[]).await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);
    let tools = plugins_uri(
        workspace_id,
        &format!("/{installation_id}/mcp/toolbox/tools"),
    );

    // 空体：`decode` 的「先看长度」分支。
    let request = Request::builder()
        .method("PUT")
        .uri(&tools)
        .header(USER_ID_HEADER, owner.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "application/json")
        .body(Body::empty())
        .expect("request");
    let (status, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_message(&body), "invalid request body", "{body}");

    // 形状不符：`tools` 不是字符串数组。
    let (status, body) = call(
        &app,
        "PUT",
        &tools,
        workspace_id,
        owner,
        Some(json!({ "tools": "search" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_message(&body), "invalid request body", "{body}");

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}

/// 前置都过了之后，出网失败必须折成**明确的 502**（`plugin_unavailable`）——
/// 而不是挂住请求、也不是 500。这是本片三条 mcp 路由里唯一真的会出网的一段。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn mcp_discovery_maps_an_unreachable_server_to_502() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let manifest = with_net_scope(manifest(
        "com.example.unreachable",
        "1.0.0",
        &mcp_hook_manifest("toolbox"),
    ));
    // `net:mcp.example.com` 与 hook 的 transport URL 精确对得上（校验器的要求），
    // 但 `mcp.example.com` 这个名字解析不到 ⇒ 发现以传输错误结束。
    let installation = install_with_net(&app, workspace_id, owner, &manifest, &[]).await;
    let installation_id = id_of(&installation).to_string();
    let version_id = version_uuid(&installation);

    let tools = plugins_uri(
        workspace_id,
        &format!("/{installation_id}/mcp/toolbox/tools"),
    );
    let (status, body) = call(&app, "GET", &tools, workspace_id, owner, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "plugin_unavailable", "{body}");
    assert_eq!(
        error_message(&body),
        "could not reach the Plugin's MCP server",
        "{body}"
    );

    cleanup_runtime(&pool, id_of(&installation), version_id).await;
    cleanup(&pool, workspace_id, &[owner]).await;
}
