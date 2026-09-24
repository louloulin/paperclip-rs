//! `/v1/storage*`（4 条）：scope 解析、配额、覆盖写与键不存在。

use super::support::*;
use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn workspace_scope_round_trips() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["storage:workspace"], "panel.js", "code").await;
    let app = app(db);

    // 空桶。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", "/v1/storage/workspace", &fixture.token, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["keys"], json!([]));

    // 不存在的键 ⇒ 404（上游 `GetStorageValue` 的 `ErrNoRows` 分支）。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", "/v1/storage/workspace/missing", &fixture.token, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "storage key not found");
    assert_eq!(body["code"], "not_found");

    // 写 ⇒ 204；读回原值；覆盖写仍然是同一条行（逻辑主键是四列，不是 id）。
    let uri = "/v1/storage/workspace/panel.cache";
    let (status, _, _) = call_raw(
        &app,
        token_req("PUT", uri, &fixture.token, Some(&json!({"value": "one"}))),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = call_raw(&app, token_req("GET", uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], "one");

    let (status, _, _) = call_raw(
        &app,
        token_req("PUT", uri, &fixture.token, Some(&json!({"value": "two"}))),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, body) = call_raw(&app, token_req("GET", uri, &fixture.token, None)).await;
    assert_eq!(body["value"], "two");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM plugin_storage WHERE installation_id = $1 AND key = 'panel.cache'",
    )
    .bind(fixture.installation_id)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(rows, 1, "覆盖写不能攒出第二行");

    // 列表给出键与**字节**大小，但**不含**值。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", "/v1/storage/workspace", &fixture.token, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let keys = body["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["key"], "panel.cache");
    assert_eq!(keys[0]["size_bytes"], 3);
    assert!(keys[0].get("value").is_none(), "列表不是批量读状态的通道");
    assert!(keys[0]["updated_at"].is_string());

    // 删 ⇒ 204；再删同键 ⇒ 404（上游代码如此）。
    let (status, _, _) = call_raw(&app, token_req("DELETE", uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = call_raw(&app, token_req("DELETE", uri, &fixture.token, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "storage key not found");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn unknown_scope_is_rejected_and_never_touches_a_row() {
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

    // 未知 scope 名 ⇒ 400（上游 `ResolveStorageScope` 的默认分支）。
    let (status, _, body) = call_raw(
        &app,
        token_req("GET", "/v1/storage/global", &fixture.token, None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");

    // 键超 1024 字节 ⇒ 507（上游 `validateStorageKey` 用的是 PluginErrorQuota）。
    let long = "k".repeat(1025);
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PUT",
            &format!("/v1/storage/workspace/{long}"),
            &fixture.token,
            Some(&json!({"value": "v"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{body}");
    assert_eq!(body["code"], "quota_exceeded");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn value_over_the_limit_is_507_not_a_silent_truncation() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let fixture = seed_panel(&pool, &db, &["storage:workspace"], "panel.js", "code").await;
    let app = app(db);

    // 恰好 102400 字节 ⇒ 通过（列上的 CHECK 也是 `<= 102400`）。
    let at_limit = "v".repeat(100 * 1024);
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PUT",
            "/v1/storage/workspace/big",
            &fixture.token,
            Some(&json!({"value": at_limit})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // 超一个字节 ⇒ 507 `quota_exceeded`（**不是** LRU 掉别人的数据）。
    let over = "v".repeat(100 * 1024 + 1);
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PUT",
            "/v1/storage/workspace/too-big",
            &fixture.token,
            Some(&json!({"value": over})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["code"], "quota_exceeded");
    assert!(
        body["detail"].as_str().unwrap().contains("100")
            || body["detail"].as_str().unwrap().contains("exceeds"),
        "{body}"
    );

    // 覆盖同一个键时用量查询**排除**这个键 ⇒ 原来的大值不会被算成「又要一份」。
    let (status, _, body) = call_raw(
        &app,
        token_req(
            "PUT",
            "/v1/storage/workspace/big",
            &fixture.token,
            Some(&json!({"value": "v".repeat(100 * 1024)})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    cleanup(&pool, fixture.workspace_id, &[fixture.user_id]).await;
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL (gate 6)"]
async fn storage_keys_are_isolated_between_installations() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let first = seed_panel(&pool, &db, &["storage:workspace"], "panel.js", "code").await;
    // 同一个 workspace 里的第二个安装：手写种子（`seed_panel` 每次都建新 workspace）。
    let manifest = panel_manifest("panel.js", &["storage:workspace"]);
    let (version_id, _) = seed_package(
        &pool,
        first.workspace_id,
        "itest-panel-2",
        &manifest,
        &[("panel.js", "code")],
    )
    .await;
    let (other_token, other_hash) = install_token();
    seed_installation(
        &pool,
        first.workspace_id,
        "itest-panel-2",
        version_id,
        &manifest,
        &["storage:workspace"],
        true,
        Some(&other_hash),
    )
    .await;
    let app = app(db);

    let (status, _, _) = call_raw(
        &app,
        token_req(
            "PUT",
            "/v1/storage/workspace/shared-name",
            &first.token,
            Some(&json!({"value": "first"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 第二个安装读同名键 ⇒ 404（键按**安装**隔离，不是按 workspace）。
    let (status, _, _) = call_raw(
        &app,
        token_req(
            "GET",
            "/v1/storage/workspace/shared-name",
            &other_token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, first.workspace_id, &[first.user_id]).await;
}
