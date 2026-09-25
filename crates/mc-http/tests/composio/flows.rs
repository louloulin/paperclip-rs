//! 端到端链路与目录/连接面（**离线替身，零 mock**）—— M8-6 的专属 `DoD`
//! （`docs/61` §4.2 / §6.5 的 M8-6 行）。
//!
//! ```text
//! connect init（路由）→ 替身发托管链接（真 HTTP）→ callback（路由）→ 落库（真 PG）
//!   → connections / toolkits（路由）→ 断开（路由）→ MCP 会话 + overlay（服务面）
//! ```
//!
//! 替身是「假 Composio」，不是「假 service」（§4.2 的替身纪律 ①）：请求走真 HTTP、真头、
//! 真 JSON；断言也逐字段查替身**收到**的原始请求（[`crate::support::calls_for`]）。

use axum::http::StatusCode;
use mc_composio::overlay::build_task_overlay;
use mc_composio::service::{ComposioConfig, ComposioService};
use mc_repos::composio::connection::ComposioConnectionRepo;
use uuid::Uuid;

use crate::support::{
    api_key_for, app_with, call, cleanup, configured_keys, connect, error_code, req_json,
    reset_calls, seed_user, send, stub_base, CALLBACK_BASE, VARIANT_BOOM, VARIANT_FOREIGN,
    VARIANT_NO_CONFIGS, VARIANT_OK,
};

/// 从替身记录的那次 `/connected_accounts/link` 请求里取出 signed state（**零 mock** 的关键：
/// 回调要的 state 是**真流程**产出的，不是测试自己签的）。
fn state_from_link_call(user: Uuid, api_key: &str) -> String {
    let calls = crate::support::calls_for(api_key, "/connected_accounts/link");
    let call = calls.last().expect("the stub must have seen a link call");
    let body: serde_json::Value = serde_json::from_str(&call.body).expect("link body json");
    let callback_url = body["callback_url"].as_str().expect("callback_url");
    assert!(
        callback_url.starts_with(&format!(
            "{CALLBACK_BASE}/api/integrations/composio/callback?state="
        )),
        "回调地址必须由配置的基址 + 冻结路径拼成：{callback_url}"
    );
    assert_eq!(
        body["auth_config_id"], "ac_notion_custom",
        "links 用的是目录归约挑出来的自定义 auth config"
    );
    assert_eq!(
        body["user_id"],
        user.to_string(),
        "composio_user_id = Multica user id"
    );
    callback_url
        .split("state=")
        .nth(1)
        .expect("state in the callback url")
        .to_string()
}

/// `DoD` 的那条端到端链：connect → callback → 落库 → toolkits（中间零 mock）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn connect_then_callback_then_persist_then_toolkits() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let api_key = api_key_for(VARIANT_OK, user);
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);
    reset_calls();

    // ① connect init：拿托管链接。
    let (status, body, _, raw) = send(
        &app,
        req_json(
            "POST",
            "/api/integrations/composio/connect/init",
            Some(user),
            &serde_json::json!({"toolkit_slug": "  Notion  "}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(
        body["redirect_url"],
        serde_json::json!(format!("https://composio.example/link/{user}"))
    );

    // ② 替身确实收到了带 x-api-key 的真请求；从它回填的 body 里取真 state。
    let state = state_from_link_call(user, &api_key);

    // ③ callback：落地。
    let (status, _, location, raw) = call(
        &app,
        "GET",
        &format!(
            "/api/integrations/composio/callback?state={state}&status=success\
             &connected_account_id=ca_live"
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND, "{raw}");
    assert_eq!(
        location.as_deref(),
        Some("/settings?tab=integrations&connected=notion")
    );

    let rows = crate::support::connection_rows(&pool, user).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["auth_config_id"], "ac_notion_custom");
    assert_eq!(rows[0]["connected_account_id"], "ca_live");

    // ④ connections：调用者自己看得到。
    let (status, body, _, raw) = call(
        &app,
        "GET",
        "/api/integrations/composio/connections",
        Some(user),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(body.as_array().expect("array").len(), 1);
    assert_eq!(body[0]["toolkit_slug"], "notion");
    assert_eq!(body[0]["status"], "active");
    assert_eq!(body[0]["last_used_at"], serde_json::Value::Null);
    let connected_at = body[0]["connected_at"].as_str().expect("connected_at");
    assert!(
        connected_at.ends_with('Z') && connected_at.contains('T'),
        "{connected_at}"
    );
    assert!(
        !raw.contains("ca_live") && !raw.contains("ac_notion_custom"),
        "响应不得含服务端内部句柄：{raw}"
    );

    // ⑤ toolkits：只有 notion（github 的 auth config 是 DISABLED ⇒ 不进目录）。
    let (status, body, _, raw) = call(
        &app,
        "GET",
        "/api/integrations/composio/toolkits",
        Some(user),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    let toolkits = body.as_array().expect("array");
    assert_eq!(
        toolkits.len(),
        1,
        "auth-config 未配的 toolkit 不出现：{raw}"
    );
    assert_eq!(toolkits[0]["slug"], "notion");
    assert_eq!(toolkits[0]["name"], "Notion");
    assert_eq!(toolkits[0]["category"], "productivity");
    assert_eq!(toolkits[0]["connectable"], serde_json::json!(true));
    assert!(toolkits[0]["logo"]
        .as_str()
        .expect("logo")
        .ends_with("/logos/notion"));

    cleanup(&pool, user).await;
}

/// connect 的三种拒绝：unknown toolkit（替身说没有 auth config）⇒ 400；
/// body 坏 / slug 空 ⇒ 400；上游 500 ⇒ 502。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn connect_init_rejects_unknown_toolkits_and_bad_bodies() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let path = "/api/integrations/composio/connect/init";

    // ① 项目里没有该 toolkit 的 auth config ⇒ 400（不是 502）。
    let app = app_with(
        db.clone(),
        configured_keys(VARIANT_NO_CONFIGS, user).await,
        true,
    );
    let (status, body, _, _) = send(
        &app,
        req_json(
            "POST",
            path,
            Some(user),
            &serde_json::json!({"toolkit_slug": "notion"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "validation_error");

    // ② body 不是 JSON / 缺 slug ⇒ 400。
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);
    for (label, body_text) in [("not json", "not json"), ("empty object", "{}")] {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("x-multica-user-id", user.to_string())
            .body(axum::body::Body::from(body_text))
            .expect("request");
        let (status, _, _, raw) = send(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {raw}");
    }

    // ③ 上游 500 ⇒ 502 upstream_error。
    let app = app_with(db.clone(), configured_keys(VARIANT_BOOM, user).await, true);
    let (status, body, _, _) = send(
        &app,
        req_json(
            "POST",
            path,
            Some(user),
            &serde_json::json!({"toolkit_slug": "notion"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "upstream_error");

    cleanup(&pool, user).await;
}

/// 目录面的两格：上游 500 ⇒ 502（**不**回落成空目录）；没有可用 auth config ⇒ 空目录 200。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn toolkits_report_upstream_failures_instead_of_an_empty_catalog() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;

    let app = app_with(db.clone(), configured_keys(VARIANT_BOOM, user).await, true);
    let (status, body, _, _) = call(
        &app,
        "GET",
        "/api/integrations/composio/toolkits",
        Some(user),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "解析失败 ⇒ 502，不是空目录"
    );
    assert_eq!(error_code(&body), "upstream_error");

    let app = app_with(
        db.clone(),
        configured_keys(VARIANT_NO_CONFIGS, user).await,
        true,
    );
    let (status, body, _, raw) = call(
        &app,
        "GET",
        "/api/integrations/composio/toolkits",
        Some(user),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert!(body.as_array().expect("array").is_empty());

    cleanup(&pool, user).await;
}

/// 断开的三格：204 + 落 `revoked`；第二次 DELETE 仍是 204（幂等 no-op）；未知 id ⇒ 404；
/// 非 uuid ⇒ 400；上游 404（连接已经没了）也当成功。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn disconnect_is_idempotent_and_hides_foreign_connections() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    let app = app_with(db.clone(), configured_keys(VARIANT_OK, user).await, true);
    reset_calls();

    // 先连一条（经回调，避免测试自己插行）。
    let state = crate::support::flow_state(&app, user).await;
    let (status, _, _, _) = call(
        &app,
        "GET",
        &format!(
            "/api/integrations/composio/callback?state={state}&status=success\
             &connected_account_id=ca_del"
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    let rows = crate::support::connection_rows(&pool, user).await;
    assert_eq!(rows.len(), 1);
    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM user_composio_connection WHERE user_id = $1 AND connected_account_id = 'ca_del'",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .expect("connection id");

    // ① 204 + revoked。
    let (status, _, _, raw) = call(
        &app,
        "DELETE",
        &format!("/api/integrations/composio/connections/{id}"),
        Some(user),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{raw}");
    let rows = crate::support::connection_rows(&pool, user).await;
    assert_eq!(rows[0]["status"], "revoked");
    let revokes = crate::support::calls_for(&api_key_for(VARIANT_OK, user), "/revoke");
    assert_eq!(revokes.len(), 1, "先撤销上游 grant");
    assert_eq!(revokes[0].method, "POST");

    // ② 第二次 ⇒ 仍是 204，且**不再**打上游（本地已经不是 active ⇒ 纯 no-op）。
    let (status, _, _, _) = call(
        &app,
        "DELETE",
        &format!("/api/integrations/composio/connections/{id}"),
        Some(user),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        crate::support::calls_for(&api_key_for(VARIANT_OK, user), "/revoke").len(),
        1,
        "重复 DELETE 不得再撤销一次"
    );

    // ③ 别人的 / 不存在的 id ⇒ 404（同判，不泄漏存在性）；非 uuid ⇒ 400。
    let unknown = Uuid::new_v4();
    for (label, path, want) in [
        (
            "unknown id",
            format!("/api/integrations/composio/connections/{unknown}"),
            StatusCode::NOT_FOUND,
        ),
        (
            "not a uuid",
            "/api/integrations/composio/connections/not-a-uuid".to_string(),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _, _, raw) = call(&app, "DELETE", &path, Some(user)).await;
        assert_eq!(status, want, "{label}: {raw}");
    }

    cleanup(&pool, user).await;
}

/// 上游已经没了（`DELETE /connected_accounts/{id}` 回 404）⇒ 客户端当成功、服务面照样落 `revoked`。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn a_404_while_removing_upstream_is_still_a_successful_disconnect() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    // `VARIANT_FOREIGN` 的替身在 `/connected_accounts/{id}` 上回 404；但归属复核也会挂
    // ⇒ 这里**直接插一行**（只考断开路径，不考回调路径）。
    let app = app_with(
        db.clone(),
        configured_keys(VARIANT_FOREIGN, user).await,
        true,
    );
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO user_composio_connection \
         (user_id, toolkit_slug, auth_config_id, connected_account_id, composio_user_id) \
         VALUES ($1, 'notion', 'ac_notion_custom', 'ca_gone', $2) RETURNING id",
    )
    .bind(user)
    .bind(user.to_string())
    .fetch_one(&pool)
    .await
    .expect("insert connection");

    let (status, _, _, raw) = call(
        &app,
        "DELETE",
        &format!("/api/integrations/composio/connections/{id}"),
        Some(user),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{raw}");
    let rows = crate::support::connection_rows(&pool, user).await;
    assert_eq!(rows[0]["status"], "revoked");

    cleanup(&pool, user).await;
}

/// 会话 + overlay（服务面，**无路由**）：真实 session URL → `{"mcpServers":{"composio":…}}`。
///
/// 这条链就是 R-M8-9 尾账要接的那一端：`create_mcp_session`（可注入的取数口）+
/// `build_task_overlay`（纯函数，anchor 的签名）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn the_service_can_turn_live_connections_into_an_overlay() {
    let Some((pool, db)) = connect().await else {
        println!("skipped: MULTICA_TEST_DATABASE_URL is not set");
        return;
    };
    let user = seed_user(&pool).await;
    stub_base().await; // 确保注入过 base（服务实例用得着）
    let repo = ComposioConnectionRepo::new(db.clone());
    let service = ComposioService::new(ComposioConfig {
        api_key: Some(api_key_for(VARIANT_OK, user)),
        state_secret: Some(crate::support::STATE_SECRET.to_string()),
        callback_base_url: Some(CALLBACK_BASE.to_string()),
        feature_enabled: true,
        api_base: mc_http::routes::composio::connect::composio_api_base(),
        state_ttl_secs: None,
    })
    .with_store(repo);

    // 没有活跃连接 ⇒ 没有会话、没有 overlay。
    assert!(service
        .create_mcp_session(mc_core::Id(user))
        .await
        .expect("session")
        .is_none());
    assert!(build_task_overlay("notion", "", &user.to_string()).is_none());

    // 插一条活跃连接 ⇒ 会话 URL 来自替身（真 HTTP）。
    sqlx::query(
        "INSERT INTO user_composio_connection \
         (user_id, toolkit_slug, auth_config_id, connected_account_id, composio_user_id) \
         VALUES ($1, 'notion', 'ac_notion_custom', 'ca_sess', $2)",
    )
    .bind(user)
    .bind(user.to_string())
    .execute(&pool)
    .await
    .expect("insert connection");

    let session = service
        .create_mcp_session(mc_core::Id(user))
        .await
        .expect("session")
        .expect("a session url");
    assert_eq!(session.url, "https://mcp.example/s/sess_1");
    let sessions =
        crate::support::calls_for(&api_key_for(VARIANT_OK, user), "/tool_router/session");
    assert_eq!(sessions.len(), 1, "会话只开一次");
    let body: serde_json::Value = serde_json::from_str(&sessions[0].body).expect("session body");
    assert_eq!(body["user_id"], user.to_string());
    assert_eq!(body["toolkits"]["enable"], serde_json::json!(["notion"]));
    assert_eq!(
        body["connected_accounts"]["notion"],
        serde_json::json!(["ca_sess"]),
        "会话按 toolkit 钉死成用户自己的账号"
    );

    let overlay = build_task_overlay("notion", &session.url, &user.to_string()).expect("overlay");
    assert_eq!(
        overlay,
        serde_json::json!({"mcpServers":{"composio":{"type":"http","url":"https://mcp.example/s/sess_1"}}})
    );

    cleanup(&pool, user).await;
}
