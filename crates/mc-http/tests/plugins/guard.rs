//! 13 条路由的**门**：这些判据与业务无关，逐条路由都要成立（上游把它们放在
//! `requirePluginsV1` + `workspaceMember`/`workspaceAdmin` 两个中间件里）。
//!
//! 顺序也是契约的一部分：**先解析 workspace、再收请求体**。所以「非法 id ⇒ 400」
//! 在 multipart 路由上不能变成「先收 2MiB 再拒」。

use super::support::*;
use super::zipfixture::zip_store;
use axum::http::StatusCode;
use serde_json::json;

/// 13 条路由的 `(method, suffix)`，`{id}` 由调用方填。
const ALL_ROUTES: &[(&str, &str)] = &[
    ("GET", ""),
    ("POST", ""),
    ("POST", "/preview"),
    ("GET", "/packages"),
    ("POST", "/packages"),
    ("POST", "/packages/local"),
    ("DELETE", "/packages/{packageId}"),
    ("PUT", "/{installationId}/config"),
    ("POST", "/{installationId}/enable"),
    ("POST", "/{installationId}/disable"),
    ("DELETE", "/{installationId}"),
    ("POST", "/{installationId}/token"),
    ("DELETE", "/{installationId}/token"),
];

fn expanded(workspace_id: &str, suffix: &str) -> String {
    let suffix = suffix
        .replace("{packageId}", "00000000-0000-0000-0000-000000000001")
        .replace("{installationId}", "00000000-0000-0000-0000-000000000002");
    format!("/api/workspaces/{workspace_id}/plugins{suffix}")
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn non_uuid_workspace_is_400_on_every_route() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let bad = "not-a-uuid";

    for (method, suffix) in ALL_ROUTES {
        let uri = expanded(bad, suffix);
        // multipart 那条走真实上传请求：判据是「400 且**没读体**」，用空归档就够。
        let (status, body) = if *method == "POST" && *suffix == "/packages" {
            let archive = zip_store(&[("multica.plugin.json", "{}")]);
            call_raw(
                &app,
                bundle_upload_req(&uri, workspace_id, user_id, "itest.zip", &archive),
            )
            .await
        } else {
            call(&app, method, &uri, workspace_id, user_id, Some(json!({}))).await
        };
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

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn non_member_is_404_and_member_who_is_not_admin_is_403() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, _admin) = seed_workspace(&pool, "owner").await;
    // 「非成员」用另一个 workspace 的 owner 扮演：对 `workspace_id` 而言它什么都不是。
    let (other_workspace, outsider) = seed_workspace(&pool, "owner").await;
    let member = seed_user(&pool, workspace_id, "member").await;

    // 非成员：workspace 面回 404（不泄露「这个 workspace 存在」），成员面也一样。
    let (status, body) = call(
        &app,
        "GET",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found");
    assert_eq!(error_message(&body), "workspace");

    // 成员：读列表可以（上游把 `GET /plugins` 放在 member 组里）。
    let (status, body) = call(
        &app,
        "GET",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugins"], json!([]));

    // 成员：写路由在 admin 组里 ⇒ 403，文案与 `routes/runtimes/access.rs` 逐字相同。
    let admin_only: &[(&str, &str)] = &[
        ("POST", ""),
        ("POST", "/preview"),
        ("GET", "/packages"),
        ("POST", "/packages/local"),
        ("DELETE", "/packages/00000000-0000-0000-0000-000000000001"),
        ("PUT", "/00000000-0000-0000-0000-000000000002/config"),
        ("POST", "/00000000-0000-0000-0000-000000000002/enable"),
        ("POST", "/00000000-0000-0000-0000-000000000002/disable"),
        ("DELETE", "/00000000-0000-0000-0000-000000000002"),
        ("POST", "/00000000-0000-0000-0000-000000000002/token"),
        ("DELETE", "/00000000-0000-0000-0000-000000000002/token"),
    ];
    for (method, suffix) in admin_only {
        let uri = plugins_uri(workspace_id, suffix);
        let (status, body) = call(&app, method, &uri, workspace_id, member, Some(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(error_code(&body), "forbidden", "{method} {uri}: {body}");
        assert_eq!(
            error_message(&body),
            "workspace admin role required",
            "{uri}"
        );
    }

    cleanup(&pool, workspace_id, &[member]).await;
    cleanup(&pool, other_workspace, &[outsider]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn disabled_plugins_v1_is_403_before_anything_else() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app_with_plugins_v1_disabled(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    // 连「非法 id」都排在开关之后（上游 handler 的第一句就是 `requirePluginsV1`）。
    let (status, body) = call(
        &app,
        "GET",
        &plugins_uri(workspace_id, ""),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "plugin_api_disabled");
    assert_eq!(error_message(&body), "Plugin management is not enabled");

    cleanup(&pool, workspace_id, &[user_id]).await;
}
